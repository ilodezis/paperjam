//! Query execution: turns the query AST and structured filters into
//! tantivy queries, runs them and assembles hits.

use std::collections::BTreeSet;
use std::ops::Bound;

use chrono::NaiveDate;
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{
    AllQuery, BooleanQuery, EmptyQuery, Occur, PhraseQuery, Query as TantivyQuery, RangeQuery,
    TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, Value as _};
use tantivy::snippet::{Snippet, SnippetGenerator};
use tantivy::tokenizer::TokenStream;
use tantivy::{DocAddress, Order, TantivyDocument, Term};

use super::query::{self, Op, Query};
use super::{
    DEFAULT_LIMIT, Filters, Hit, Inner, MAX_LIMIT, SearchPage, SearchRequest, Sort, StoredMeta,
    analyzers, days, ident, internal,
};
use crate::error::{Result, ServiceError};
use crate::ids::{DocumentId, PersonId};
use crate::matching;

type BoxQuery = Box<dyn TantivyQuery>;

/// Length of a snippet in characters.
const SNIPPET_CHARS: usize = 160;
/// A summary is one to three sentences: shown nearly whole.
const SUMMARY_SNIPPET_CHARS: usize = 300;
/// `has:expiring` finds documents due within this many days (or already past
/// due).
const EXPIRING_DAYS: i64 = 270;
/// Longest term the index holds (see `analyzers::MAX_TOKEN_BYTES`).
const MAX_TERM_BYTES: usize = 80;
/// Markers that exist as `has:` names (and the words that map onto them).
const HAS_NAMES: [(&str, &str); 11] = [
    ("attention", "attention"),
    ("date", "date"),
    ("due", "due"),
    ("tags", "tags"),
    ("tag", "tags"),
    ("people", "people"),
    ("person", "people"),
    ("summary", "summary"),
    ("type", "type"),
    ("from", "from"),
    ("correspondent", "from"),
];

fn bad_filter(message: impl Into<String>) -> ServiceError {
    ServiceError::invalid("query_filter", message)
}

struct Builder<'a> {
    inner: &'a Inner,
    /// Keys of the fields that hold dates (see [`super::SearchIndex::search_with`]).
    date_fields: &'a BTreeSet<String>,
    /// Text queries that are not under a negation, for snippet highlighting.
    positive: Vec<BoxQuery>,
}

pub(super) fn search(
    inner: &Inner,
    request: &SearchRequest,
    date_fields: &BTreeSet<String>,
) -> Result<SearchPage> {
    let parsed = request
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(query::parse)
        .transpose()?;
    let page = run(inner, request, date_fields, parsed.as_ref())?;
    // Nothing matches every word: show what matches some of them, and say so.
    if page.total == 0
        && request.cursor.is_none()
        && let Some(relaxed) = parsed.as_ref().and_then(relaxed)
    {
        let mut page = run(inner, request, date_fields, Some(&relaxed))?;
        page.partial = page.total > 0;
        return Ok(page);
    }
    Ok(page)
}

/// `query` with its free-text words joined by OR instead of AND, when it is a
/// plain list of terms (two or more words, maybe with filters and negations
/// beside them). Anything with an explicit `OR` or parentheses is the
/// caller's own structure and is left as it is. A word with a digit in it is
/// a number or an identifier, looked up whole: "some of the words" would only
/// bring other people's documents, so such a query stays as it is too.
fn relaxed(query: &Query) -> Option<Query> {
    let Query::And(items) = query else {
        return None;
    };
    let (words, rest): (Vec<Query>, Vec<Query>) = items
        .iter()
        .cloned()
        .partition(|item| matches!(item, Query::Term(_) | Query::Phrase(_)));
    let has_digit = |item: &Query| matches!(item, Query::Term(text) | Query::Phrase(text) if text.chars().any(|c| c.is_ascii_digit()));
    if words.len() < 2 || words.iter().any(has_digit) {
        return None;
    }
    let mut parts = vec![Query::Or(words)];
    parts.extend(rest);
    Some(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Query::And(parts)
    })
}

fn run(
    inner: &Inner,
    request: &SearchRequest,
    date_fields: &BTreeSet<String>,
    text: Option<&Query>,
) -> Result<SearchPage> {
    let limit = request.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let offset = parse_cursor(request.cursor.as_deref())?;
    let mut builder = Builder {
        inner,
        date_fields,
        positive: Vec::new(),
    };
    let f = &inner.fields;

    let has_text = text.is_some();
    let mut clauses: Vec<(Occur, BoxQuery)> = Vec::new();
    match text {
        Some(q) => clauses.push((Occur::Must, builder.build(q, false)?)),
        None => clauses.push((Occur::Must, Box::new(AllQuery))),
    }
    for filter in builder.structured(&request.filters)? {
        clauses.push((Occur::Must, filter));
    }
    if !request.filters.include_trashed {
        clauses.push((Occur::Must, flag(f.trashed, 0)));
    }
    let full = BooleanQuery::new(clauses);

    let searcher = inner.reader.searcher();
    let top = TopDocs::with_limit(limit).and_offset(offset);
    let (total, addresses): (usize, Vec<DocAddress>) = match (request.sort, has_text) {
        (Sort::Relevance, true) => {
            let (total, hits) = searcher
                .search(&full, &(Count, top.order_by_score()))
                .map_err(internal)?;
            (total, hits.into_iter().map(|(_, a)| a).collect())
        }
        (sort, _) => {
            let (field, order) = match sort {
                Sort::DateDesc => ("date_sort_desc", Order::Desc),
                Sort::DateAsc => ("date_sort_asc", Order::Asc),
                // Without a text query every hit scores the same: newest first.
                Sort::Relevance => ("added_ts", Order::Desc),
            };
            let collector = top.order_by_fast_field::<i64>(field, order);
            let (total, hits) = searcher
                .search(&full, &(Count, collector))
                .map_err(internal)?;
            (total, hits.into_iter().map(|(_, a)| a).collect())
        }
    };

    let generators = if builder.positive.is_empty() {
        Vec::new()
    } else {
        let positive = BooleanQuery::new(
            builder
                .positive
                .iter()
                .map(|q| (Occur::Should, q.box_clone()))
                .collect(),
        );
        f.stems()
            .map(|s| s.field)
            .chain([f.text_exact])
            .map(|field| {
                let mut generator =
                    SnippetGenerator::create(&searcher, &positive, field).map_err(internal)?;
                generator.set_max_num_chars(SNIPPET_CHARS);
                Ok(generator)
            })
            .collect::<Result<Vec<_>>>()?
    };

    let mut hits = Vec::with_capacity(addresses.len());
    for address in addresses {
        let doc: TantivyDocument = searcher.doc(address).map_err(internal)?;
        let text_of = |field: Field| doc.get_first(field).and_then(|v| v.as_str());
        let id: DocumentId = text_of(f.id)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| internal("hit without id"))?;
        let meta: StoredMeta = serde_json::from_str(text_of(f.meta).unwrap_or("{}"))
            .map_err(|e| internal(format!("unreadable hit data: {e}")))?;
        let body = text_of(f.body).unwrap_or("");
        let snippet = generators
            .iter()
            .map(|g| g.snippet(body))
            .find(|s| !s.highlighted().is_empty())
            .map_or_else(
                || unhighlighted_snippet(meta.summary.as_deref(), body, generators.is_empty()),
                |s| render_snippet_in(&s, body),
            );
        hits.push(Hit {
            id,
            title: meta.title,
            type_key: meta.type_key,
            document_date: meta.document_date,
            due_date: meta.due_date,
            organizations: meta.organizations,
            people: meta.people,
            key_fields: meta.key_fields,
            snippet,
            trashed: meta.trashed,
            attention: meta.attention,
            has_pages: meta.has_pages.unwrap_or(true),
        });
    }

    let seen = offset + hits.len();
    Ok(SearchPage {
        next_cursor: (seen < total).then(|| format!("o{seen}")),
        total: total as u64,
        hits,
        partial: false,
    })
}

fn parse_cursor(cursor: Option<&str>) -> Result<usize> {
    let Some(cursor) = cursor else { return Ok(0) };
    cursor
        .strip_prefix('o')
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| ServiceError::invalid("invalid_cursor", "the cursor is not valid"))
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

fn render_snippet(snippet: &Snippet) -> String {
    let fragment = snippet.fragment();
    let mut out = String::with_capacity(fragment.len() + 16);
    let mut at = 0;
    for range in snippet.highlighted() {
        if range.start < at || range.end > fragment.len() {
            continue;
        }
        escape(&fragment[at..range.start], &mut out);
        out.push_str("<mark>");
        escape(&fragment[range.clone()], &mut out);
        out.push_str("</mark>");
        at = range.end;
    }
    escape(&fragment[at..], &mut out);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A fragment of `body` with an ellipsis on each side where the text goes on.
fn render_snippet_in(snippet: &Snippet, body: &str) -> String {
    let fragment = snippet.fragment().trim();
    let mut out = render_snippet(snippet);
    if !body.trim_start().starts_with(fragment) {
        out.insert_str(0, "… ");
    }
    if !body.trim_end().ends_with(fragment) {
        out.push_str(" …");
    }
    out
}

/// What a hit shows when no fragment matched. With no text query at all the
/// summary reads better than the first lines of a scan; with one, keep the
/// start of the text as before.
fn unhighlighted_snippet(summary: Option<&str>, body: &str, no_text_query: bool) -> String {
    summary
        .filter(|_| no_text_query)
        .map(|s| plain_snippet(s, SUMMARY_SNIPPET_CHARS))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| plain_snippet(body, SNIPPET_CHARS))
}

/// The start of `body`, cut after a whole word: a word cut in half reads as
/// a typo.
fn plain_snippet(body: &str, limit: usize) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    if collapsed.chars().count() <= limit {
        escape(&collapsed, &mut out);
        return out;
    }
    let head: String = collapsed.chars().take(limit + 1).collect();
    let cut = head
        .rfind(' ')
        .filter(|&i| i > 0)
        .unwrap_or_else(|| head.char_indices().last().map_or(0, |(i, _)| i));
    escape(
        head[..cut].trim_end_matches([',', ';', ':', '—', '-', ' ']),
        &mut out,
    );
    out.push_str(" …");
    out
}

fn flag(field: Field, value: u64) -> BoxQuery {
    Box::new(TermQuery::new(
        Term::from_field_u64(field, value),
        IndexRecordOption::Basic,
    ))
}

fn string_term(field: Field, value: &str) -> BoxQuery {
    Box::new(TermQuery::new(
        Term::from_field_text(field, value),
        IndexRecordOption::Basic,
    ))
}

fn any_of(queries: Vec<BoxQuery>) -> BoxQuery {
    match queries.len() {
        0 => Box::new(EmptyQuery),
        _ => Box::new(BooleanQuery::new(
            queries.into_iter().map(|q| (Occur::Should, q)).collect(),
        )),
    }
}

fn all_of(queries: Vec<BoxQuery>) -> BoxQuery {
    match queries.len() {
        0 => Box::new(EmptyQuery),
        _ => Box::new(BooleanQuery::new(
            queries.into_iter().map(|q| (Occur::Must, q)).collect(),
        )),
    }
}

fn not(query: BoxQuery) -> BoxQuery {
    Box::new(BooleanQuery::new(vec![
        (Occur::Must, Box::new(AllQuery) as BoxQuery),
        (Occur::MustNot, query),
    ]))
}

/// The half-open day range `[start, end)` a partial date stands for.
fn partial_date(value: &str) -> Option<(NaiveDate, NaiveDate)> {
    let parts: Vec<&str> = value.split('-').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let number = |i: usize| parts.get(i).and_then(|p| p.parse::<u32>().ok());
    let year = i32::try_from(number(0)?)
        .ok()
        .filter(|_| parts[0].len() == 4)?;
    match parts.len() {
        1 => Some((
            NaiveDate::from_ymd_opt(year, 1, 1)?,
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?,
        )),
        2 => {
            let month = number(1)?;
            let start = NaiveDate::from_ymd_opt(year, month, 1)?;
            let end = if month == 12 {
                NaiveDate::from_ymd_opt(year + 1, 1, 1)?
            } else {
                NaiveDate::from_ymd_opt(year, month + 1, 1)?
            };
            Some((start, end))
        }
        3 => {
            let start = NaiveDate::from_ymd_opt(year, number(1)?, number(2)?)?;
            Some((start, start.succ_opt()?))
        }
        _ => None,
    }
}

/// Bounds for `op value` where the value stands for `[start, end)`; a plain
/// number has `end == start` and is compared as a point.
fn bounds(op: Op, start: f64, end: f64) -> (Bound<f64>, Bound<f64>) {
    let point = start == end;
    match op {
        Op::Eq if point => (Bound::Included(start), Bound::Included(start)),
        Op::Eq => (Bound::Included(start), Bound::Excluded(end)),
        Op::Ge => (Bound::Included(start), Bound::Unbounded),
        Op::Gt if point => (Bound::Excluded(start), Bound::Unbounded),
        Op::Gt => (Bound::Included(end), Bound::Unbounded),
        Op::Le if point => (Bound::Unbounded, Bound::Included(start)),
        Op::Le => (Bound::Unbounded, Bound::Excluded(end)),
        Op::Lt => (Bound::Unbounded, Bound::Excluded(start)),
    }
}

fn range((lower, upper): (Bound<f64>, Bound<f64>), term: impl Fn(f64) -> Term) -> BoxQuery {
    Box::new(RangeQuery::new(lower.map(&term), upper.map(&term)))
}

fn day_range(field: Field, op: Op, start: NaiveDate, end: NaiveDate) -> BoxQuery {
    range(bounds(op, days(start) as f64, days(end) as f64), |d| {
        Term::from_field_i64(field, d as i64)
    })
}

impl Builder<'_> {
    fn tokens(&self, analyzer: &str, text: &str) -> Vec<String> {
        let mut analyzer = self
            .inner
            .index
            .tokenizers()
            .get(analyzer)
            .expect("analyzers are registered at open");
        let mut stream = analyzer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().text.clone());
        }
        tokens
    }

    /// Free text: any language field may match; several tokens must be
    /// adjacent (a phrase), which also keeps `112-233-445` together.
    fn text(&mut self, text: &str, negated: bool) -> BoxQuery {
        let f = &self.inner.fields;
        let mut alternatives: Vec<BoxQuery> = Vec::new();
        let languages = f
            .stems()
            .map(|s| (s.field, s.analyzer))
            .chain([(f.text_exact, analyzers::EXACT)])
            .collect::<Vec<_>>();
        for (field, analyzer) in languages {
            let mut terms: Vec<Term> = self
                .tokens(analyzer, text)
                .iter()
                .map(|t| Term::from_field_text(field, t))
                .collect();
            match terms.len() {
                0 => {}
                1 => alternatives.push(Box::new(TermQuery::new(
                    terms.remove(0),
                    IndexRecordOption::WithFreqs,
                ))),
                _ => alternatives.push(Box::new(PhraseQuery::new(terms))),
            }
        }
        let words = self.tokens(analyzers::EXACT, text);
        if let Some(glued) = self.glued(&words) {
            alternatives.push(glued);
        }
        let query = any_of(alternatives);
        if !negated {
            self.positive.push(query.box_clone());
        }
        query
    }

    /// The words glued into one identifier (`AP`, `123` → `AP123`), looked up
    /// where documents hold identifiers whole (`text_exact`) and where they
    /// hold spaced ones glued (`text_ident`). `None` unless the words can be
    /// an identifier.
    fn glued(&self, words: &[String]) -> Option<BoxQuery> {
        let glued = words.concat();
        if words.is_empty()
            || glued.len() > MAX_TERM_BYTES
            || !ident::has_digit(&glued)
            || !words.iter().all(|w| ident::is_part(w))
        {
            return None;
        }
        let f = &self.inner.fields;
        Some(any_of(vec![
            Box::new(TermQuery::new(
                Term::from_field_text(f.text_exact, &glued),
                IndexRecordOption::WithFreqs,
            )),
            Box::new(TermQuery::new(
                Term::from_field_text(f.text_ident, &glued),
                IndexRecordOption::Basic,
            )),
        ]))
    }

    /// Consecutive plain terms that may be one identifier typed with spaces
    /// (`4521 123456`, `AP 1234567`): each term on its own, or all the words
    /// glued together.
    fn spaced_run(&mut self, items: &[Query], negated: bool) -> Result<BoxQuery> {
        let mut words: Vec<String> = Vec::new();
        let mut separately: Vec<BoxQuery> = Vec::new();
        for item in items {
            if let Query::Term(text) = item {
                words.extend(self.tokens(analyzers::EXACT, text));
            }
            separately.push(self.build(item, negated)?);
        }
        let mut alternatives = vec![all_of(separately)];
        if let Some(glued) = self.glued(&words) {
            if !negated {
                self.positive.push(glued.box_clone());
            }
            alternatives.push(glued);
        }
        Ok(any_of(alternatives))
    }

    fn build(&mut self, q: &Query, negated: bool) -> Result<BoxQuery> {
        Ok(match q {
            Query::Term(text) | Query::Phrase(text) => self.text(text, negated),
            Query::Not(inner) => not(self.build(inner, !negated)?),
            Query::And(items) => {
                let words: Vec<Option<Vec<String>>> = items
                    .iter()
                    .map(|item| match item {
                        Query::Term(text) => Some(self.tokens(analyzers::EXACT, text)),
                        _ => None,
                    })
                    .collect();
                let runs = ident::spaced_runs(&words);
                let mut clauses: Vec<(Occur, BoxQuery)> = Vec::new();
                let mut next = 0;
                while next < items.len() {
                    if let Some(run) = runs.iter().find(|r| r.start == next) {
                        let spaced = self.spaced_run(&items[run.clone()], negated)?;
                        clauses.push((Occur::Must, spaced));
                        next = run.end;
                        continue;
                    }
                    match &items[next] {
                        Query::Not(inner) => {
                            clauses.push((Occur::MustNot, self.build(inner, !negated)?));
                        }
                        other => clauses.push((Occur::Must, self.build(other, negated)?)),
                    }
                    next += 1;
                }
                if clauses.iter().all(|(occur, _)| *occur == Occur::MustNot) {
                    clauses.push((Occur::Must, Box::new(AllQuery)));
                }
                Box::new(BooleanQuery::new(clauses))
            }
            Query::Or(items) => any_of(
                items
                    .iter()
                    .map(|item| self.build(item, negated))
                    .collect::<Result<_>>()?,
            ),
            Query::Filter { key, op, value } => self.filter(key, *op, value)?,
            Query::Has(name) => self.has(name),
        })
    }

    /// Due within [`EXPIRING_DAYS`] days of today or already past due. Today
    /// is read when the query runs: what is "soon" changes every day, so it
    /// is not something the index could hold.
    fn expiring(&self) -> BoxQuery {
        let horizon = days(chrono::Utc::now().date_naive()) + EXPIRING_DAYS;
        Box::new(RangeQuery::new(
            Bound::Unbounded,
            Bound::Included(Term::from_field_i64(self.inner.fields.due_day, horizon)),
        ))
    }

    fn has(&self, name: &str) -> BoxQuery {
        if name == "expiring" {
            return self.expiring();
        }
        let marker = HAS_NAMES
            .iter()
            .find(|(word, _)| *word == name)
            .map(|(_, marker)| (*marker).to_string())
            .unwrap_or_else(|| {
                if name.starts_with("field.") {
                    name.to_string()
                } else {
                    format!("field.{name}")
                }
            });
        string_term(self.inner.fields.has, &marker)
    }

    /// All folded tokens of `value` must appear among the names.
    fn names(&self, field: Field, value: &str) -> BoxQuery {
        let folded = matching::normalize(value);
        all_of(
            self.tokens(analyzers::NORM, &folded)
                .iter()
                .map(|t| string_term(field, t))
                .collect(),
        )
    }

    fn filter(&self, key: &str, op: Op, value: &str) -> Result<BoxQuery> {
        let f = &self.inner.fields;
        let eq_only = || {
            if op == Op::Eq {
                Ok(())
            } else {
                Err(bad_filter(format!("{key}: takes no comparison operator")))
            }
        };
        Ok(match key {
            "type" => {
                eq_only()?;
                string_term(f.type_key, &value.to_lowercase())
            }
            "from" => {
                eq_only()?;
                self.names(f.from_text, value)
            }
            "person" => {
                eq_only()?;
                if value.parse::<PersonId>().is_ok() {
                    string_term(f.person_id, &value.to_lowercase())
                } else {
                    self.names(f.person_text, value)
                }
            }
            "tag" => {
                eq_only()?;
                string_term(f.tag, &value.to_lowercase())
            }
            "date" | "added" | "due" => {
                let (start, end) = partial_date(value).ok_or_else(|| {
                    bad_filter(format!(
                        "{key}: expected a date like 2024, 2024-03 or 2024-03-15"
                    ))
                })?;
                let field = match key {
                    "date" => f.document_date,
                    "due" => f.due_day,
                    _ => f.added_day,
                };
                day_range(field, op, start, end)
            }
            "local" => {
                eq_only()?;
                let on = match value.to_lowercase().as_str() {
                    "yes" | "true" | "1" | "on" => 1,
                    "no" | "false" | "0" | "off" => 0,
                    _ => return Err(bad_filter("local: use yes or no")),
                };
                flag(f.local_only, on)
            }
            other => match other.strip_prefix("field.") {
                Some(field_key) => self.json_filter(field_key, op, value)?,
                None => return Err(bad_filter(format!("unknown filter {other:?}"))),
            },
        })
    }

    /// `field.<key>` comparisons: a date, then a number, then exact text. A bare
    /// year is a date in a field that holds dates (`<2037` is before 2037,
    /// `=2037` is any day of it), a number elsewhere.
    fn json_filter(&self, key: &str, op: Op, value: &str) -> Result<BoxQuery> {
        let field = self.inner.fields.fields;
        let numeric = |v: f64| {
            let mut term = Term::from_field_json_path(field, key, false);
            term.append_type_and_fast_value(v);
            term
        };
        // A bare year is a number, unless the field holds dates: then it is that year.
        let bare_year = self.date_fields.contains(key);
        if (value.contains('-') || bare_year)
            && let Some((start, end)) = partial_date(value)
        {
            return Ok(range(
                bounds(op, days(start) as f64, days(end) as f64),
                numeric,
            ));
        }
        let text_term = |token: &str| {
            let mut term = Term::from_field_json_path(field, key, false);
            term.append_type_and_str(token);
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as BoxQuery
        };
        let tokens = self.tokens(analyzers::EXACT, value);
        if let Ok(number) = value.parse::<f64>()
            && number.is_finite()
        {
            let as_number = range(bounds(op, number, number), numeric);
            if op != Op::Eq || tokens.is_empty() {
                return Ok(as_number);
            }
            // An identifier made of digits (a passport number, `4521` for the
            // series `45 21`) is stored as text, not as a number.
            let as_text = all_of(tokens.iter().map(|t| text_term(t)).collect());
            return Ok(any_of(vec![as_number, as_text]));
        }
        if op != Op::Eq {
            return Err(bad_filter(format!(
                "field.{key}: comparisons need a number or a date"
            )));
        }
        let word_by_word = all_of(tokens.iter().map(|t| text_term(t)).collect());
        // Identifiers are stored without separators: `AP 123` is `AP123`.
        let glued = tokens.concat();
        if tokens.len() < 2 || glued.len() > MAX_TERM_BYTES {
            return Ok(word_by_word);
        }
        Ok(any_of(vec![word_by_word, text_term(&glued)]))
    }

    fn structured(&self, filters: &Filters) -> Result<Vec<BoxQuery>> {
        let f = &self.inner.fields;
        let mut out: Vec<BoxQuery> = Vec::new();
        if !filters.types.is_empty() {
            out.push(any_of(
                filters
                    .types
                    .iter()
                    .map(|t| string_term(f.type_key, &t.to_lowercase()))
                    .collect(),
            ));
        }
        if !filters.people.is_empty() {
            out.push(any_of(
                filters
                    .people
                    .iter()
                    .map(|p| string_term(f.person_id, &p.to_string()))
                    .collect(),
            ));
        }
        if !filters.correspondents.is_empty() {
            out.push(any_of(
                filters
                    .correspondents
                    .iter()
                    .map(|c| string_term(f.correspondent_id, &c.to_string()))
                    .collect(),
            ));
        }
        for tag in &filters.tags {
            out.push(string_term(f.tag, &tag.to_lowercase()));
        }
        if filters.date_from.is_some() || filters.date_to.is_some() {
            let lower = filters.date_from.map_or(Bound::Unbounded, |d| {
                Bound::Included(Term::from_field_i64(f.document_date, days(d)))
            });
            let upper = filters.date_to.map_or(Bound::Unbounded, |d| {
                Bound::Included(Term::from_field_i64(f.document_date, days(d)))
            });
            out.push(Box::new(RangeQuery::new(lower, upper)));
        }
        match filters.has_attention {
            Some(true) => out.push(self.has("attention")),
            Some(false) => out.push(not(self.has("attention"))),
            None => {}
        }
        if let Some(local) = filters.local_only {
            out.push(flag(f.local_only, u64::from(local)));
        }
        for filter in &filters.fields {
            out.push(self.json_filter(&filter.key, filter.op, &filter.value)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod snippet_tests {
    use super::*;

    #[test]
    fn a_plain_snippet_ends_on_a_whole_word() {
        let body = "Паспорт выдан Дата выдачи 22.10.2021 Код подразделения 770-010 ".repeat(5);
        let snippet = plain_snippet(&body, SNIPPET_CHARS);
        assert!(snippet.ends_with(" …"), "{snippet}");
        let last = snippet.trim_end_matches(" …").rsplit(' ').next().unwrap();
        assert!(body.split_whitespace().any(|w| w == last), "{snippet}");
        assert!(snippet.chars().count() <= SNIPPET_CHARS + 2);
    }

    #[test]
    fn a_short_body_is_shown_whole() {
        assert_eq!(
            plain_snippet("Счёт  №  15\nот 1 мая", SNIPPET_CHARS),
            "Счёт № 15 от 1 мая"
        );
    }

    #[test]
    fn no_query_shows_the_summary_escaped_and_whole() {
        let snippet = unhighlighted_snippet(
            Some(
                "Lease <1>
 for  a flat.",
            ),
            "body text",
            true,
        );
        assert_eq!(snippet, "Lease &lt;1&gt; for a flat.");
    }

    #[test]
    fn a_long_summary_is_cut_on_a_whole_word() {
        let summary = "alpha beta ".repeat(60);
        let snippet = unhighlighted_snippet(Some(&summary), "body", true);
        assert!(snippet.ends_with("beta …"), "{snippet}");
        assert!(snippet.chars().count() <= SUMMARY_SNIPPET_CHARS + 2);
    }

    #[test]
    fn no_summary_falls_back_to_the_start_of_the_text() {
        assert_eq!(unhighlighted_snippet(None, "Счёт 15", true), "Счёт 15");
        assert_eq!(
            unhighlighted_snippet(Some("  "), "Счёт 15", true),
            "Счёт 15"
        );
    }

    #[test]
    fn a_text_query_keeps_the_start_of_the_text() {
        assert_eq!(
            unhighlighted_snippet(Some("Summary"), "Счёт 15", false),
            "Счёт 15"
        );
    }

    #[test]
    fn one_endless_word_is_still_cut() {
        let snippet = plain_snippet(&"x".repeat(400), SNIPPET_CHARS);
        assert!(snippet.ends_with(" …"));
        assert!(snippet.chars().count() <= SNIPPET_CHARS + 2);
    }
}
