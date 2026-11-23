import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, ChevronRight, LoaderCircle, Plus, X } from "lucide-react";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, type FieldDef, type FieldKind, type TypeView } from "../api";
import { invalidateArchive, taxonomyQuery } from "../queries";
import { SelectDropdown } from "./controls/SelectDropdown";
import { toast } from "./controls/toast";

const fieldKinds: FieldKind[] = ["text", "date", "money", "number", "identifier", "list"];
const KNOWN_VALIDATORS = [
  "personal_number",
  "iban",
  "mrz",
  "inn",
  "snils",
  "cnp",
  "idnp",
  "pesel",
  "bsn",
  "nir",
  "steuer_id",
  "dni_nie",
  "cpf",
  "aadhaar",
  "nhs",
] as const;

type TypeFilter = "used" | "all";

function normalizeFields(list: FieldDef[]): FieldDef[] {
  return list.map((field) => ({
    key: field.key,
    labels: { en: field.labels.en ?? "", ru: field.labels.ru ?? "" },
    kind: field.kind,
    validator: field.kind === "identifier" ? (field.validator || null) : null,
    required: field.required,
    ...(field.kind === "text" && field.issuer ? { issuer: true } : {}),
  }));
}

function typeName(item: TypeView, language: string): string {
  return item.labels[language] ?? item.labels.en ?? item.key;
}

function errorCode(cause: unknown): string {
  return cause instanceof ApiError ? cause.code : "network";
}

function generatedKey(label: string): string {
  const key = label.normalize("NFKD").replace(/[̀-ͯ]/g, "").toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "");
  return /^[a-z]/.test(key) ? key : `type_${key || "custom"}`;
}

function nextFieldKey(fields: FieldDef[]): string {
  let number = fields.length + 1;
  while (fields.some((field) => field.key === `field_${number}`)) number += 1;
  return `field_${number}`;
}

export function DocumentTypesSettings() {
  const { t, i18n } = useTranslation();
  const { data: taxonomy, error: queryError, refetch } = useQuery(taxonomyQuery);
  const [filter, setFilter] = useState<TypeFilter>("used");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const language = i18n.resolvedLanguage ?? "en";
  const selected = taxonomy?.types.find((item) => item.id === selectedId);
  const visibleTypes = taxonomy?.types.filter((item) => filter === "all" || item.document_count > 0) ?? [];
  const groups = [
    { id: "builtin", label: t("documentTypes.builtinGroup"), items: visibleTypes.filter((item) => item.builtin) },
    { id: "custom", label: t("documentTypes.customGroup"), items: visibleTypes.filter((item) => !item.builtin) },
  ].filter((group) => group.items.length > 0);

  function showList() {
    setSelectedId(null);
    setCreating(false);
  }

  return (
    <section className="settings-section document-types" aria-labelledby="document-types-heading">
      <div className="settings-section-heading">
        <div><h2 id="document-types-heading" className="settings-pane-title">{t("documentTypes.title")}</h2><p>{t("documentTypes.description")}</p></div>
        {!creating && !selected ? <button type="button" className="button button--secondary" disabled={!taxonomy} onClick={() => setCreating(true)}><Plus aria-hidden="true" />{t("taxonomy.add.types")}</button> : null}
      </div>

      {queryError ? <div className="document-type-error" role="alert"><span>{t(`errors.${errorCode(queryError)}`, { defaultValue: t("errors.taxonomy") })}</span><button type="button" className="quiet-button" onClick={() => void refetch()}>{t("common.retry")}</button></div> : null}
      {!taxonomy && !queryError ? <div className="document-type-loading" role="status"><LoaderCircle className="spin" aria-hidden="true" />{t("taxonomy.loading")}</div> : null}

      {creating ? <CreateDocumentType onCreated={() => { setCreating(false); setFilter("all"); }} onCancel={() => setCreating(false)} /> : null}
      {selected ? <TypeEditor key={selected.id} item={selected} language={language} onBack={showList} /> : null}

      {taxonomy && !creating && !selected ? (
        <>
          <div className="document-type-filter" role="group" aria-label={t("documentTypes.views")}>
            <button type="button" aria-label={t("documentTypes.inUse")} aria-pressed={filter === "used"} onClick={() => setFilter("used")}>{t("documentTypes.inUse")}</button>
            <button type="button" aria-label={t("documentTypes.all")} aria-pressed={filter === "all"} onClick={() => setFilter("all")}>{t("documentTypes.all")}</button>
          </div>
          {groups.length > 0 ? (
            <div className="document-type-list">
              {groups.map((group) => (
                <section key={group.id} className="document-type-group" aria-labelledby={`document-type-group-${group.id}`}>
                  <h3 id={`document-type-group-${group.id}`}>{group.label}</h3>
                  <ul>
                    {group.items.map((item) => (
                      <li key={item.id}>
                        <div><strong>{typeName(item, language)}</strong><span>{t("taxonomy.fieldCount", { count: item.fields.length })} · {t("taxonomy.usedBy", { count: item.document_count })}</span></div>
                        <button type="button" className="quiet-button" aria-label={t("documentTypes.customizeFor", { name: typeName(item, language) })} onClick={() => setSelectedId(item.id)}>{t("documentTypes.customize")}<ChevronRight aria-hidden="true" /></button>
                      </li>
                    ))}
                  </ul>
                </section>
              ))}
            </div>
          ) : <p className="settings-note">{t(filter === "used" ? "documentTypes.emptyUsed" : "documentTypes.emptyAll")}</p>}
        </>
      ) : null}
    </section>
  );
}

function CreateDocumentType({ onCreated, onCancel }: { onCreated: () => void; onCancel: () => void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [english, setEnglish] = useState("");
  const [russian, setRussian] = useState("");
  const [key, setKey] = useState("");
  const [expert, setExpert] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function create(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.createType({ key: expert ? key : generatedKey(english), labels: { en: english, ru: russian }, fields: [] });
      await invalidateArchive(queryClient);
      toast(t("documentTypes.createdToast"), { icon: "check" });
      onCreated();
    } catch (cause) {
      setError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="document-type-create" onSubmit={create}>
      <div className="taxonomy-label-grid">
        <label className="field-label"><span>{t("taxonomy.englishLabel")}</span><input required autoFocus value={english} onChange={(event) => setEnglish(event.target.value)} aria-label={t("taxonomy.newName.types")} /></label>
        <label className="field-label"><span>{t("taxonomy.russianLabel")}</span><input required value={russian} onChange={(event) => setRussian(event.target.value)} aria-label={t("taxonomy.newRussianType")} /></label>
      </div>
      <label className="required-control document-type-expert"><input type="checkbox" role="switch" className="switch" checked={expert} onChange={(event) => setExpert(event.target.checked)} aria-label={t("documentTypes.expertMode")} /><span>{t("documentTypes.expertMode")}</span></label>
      {expert ? <label className="field-label document-type-key"><span>{t("taxonomy.typeKey")}</span><input required pattern="[a-z][a-z0-9_]*" placeholder={t("taxonomy.keyPlaceholder")} value={key} onChange={(event) => setKey(event.target.value)} aria-label={t("taxonomy.newTypeKey")} /></label> : null}
      {error ? <div className="document-type-error" role="alert">{t(`errors.${error}`, { defaultValue: t("errors.taxonomy") })}</div> : null}
      <div className="document-type-create-actions">
        <button type="submit" className="button button--primary" disabled={busy}>{t("taxonomy.createSubmit.types")}</button>
        <button type="button" className="button button--secondary" disabled={busy} onClick={onCancel}>{t("common.cancel")}</button>
      </div>
    </form>
  );
}

function TypeEditor({ item, language, onBack }: { item: TypeView; language: string; onBack: () => void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [english, setEnglish] = useState(item.labels.en ?? "");
  const [russian, setRussian] = useState(item.labels.ru ?? "");
  const [fields, setFields] = useState<FieldDef[]>(() => item.fields.map((field) => ({ ...field, labels: { ...field.labels } })));
  const [expert, setExpert] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [baseline, setBaseline] = useState(() => JSON.stringify({ labels: { en: item.labels.en ?? "", ru: item.labels.ru ?? "" }, fields: normalizeFields(item.fields) }));
  const payload = { labels: { en: english, ru: russian }, fields: normalizeFields(fields) };
  const dirty = JSON.stringify(payload) !== baseline;
  const name = typeName(item, language);

  function updateField(index: number, patch: Partial<FieldDef>) {
    setFields((current) => current.map((field, position) => position === index ? { ...field, ...patch } : field));
  }

  function updateLabel(index: number, locale: "en" | "ru", value: string) {
    const field = fields[index];
    updateField(index, { labels: { ...field.labels, [locale]: value } });
  }

  async function save(event: FormEvent) {
    event.preventDefault();
    const saving = { labels: { en: english, ru: russian }, fields: normalizeFields(fields) };
    setBusy(true);
    setError(null);
    try {
      await api.updateType(item.id, saving);
      setBaseline(JSON.stringify(saving));
      await invalidateArchive(queryClient);
      toast(t("taxonomy.savedToast"), { icon: "check" });
    } catch (cause) {
      setError(errorCode(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="document-type-detail">
      <button type="button" className="quiet-button document-type-back" aria-label={t("documentTypes.back")} onClick={onBack}><ArrowLeft aria-hidden="true" />{t("documentTypes.title")}</button>
      <h3 className="document-type-detail-title">{name}</h3>
      <form className="taxonomy-editor type-editor" onSubmit={save}>
        <div className="taxonomy-label-grid">
          <label className="field-label"><span>{t("taxonomy.englishLabel")}</span><input required value={english} onChange={(event) => setEnglish(event.target.value)} aria-label={t("taxonomy.englishLabelFor", { name: item.labels.en ?? item.key })} /></label>
          <label className="field-label"><span>{t("taxonomy.russianLabel")}</span><input required value={russian} onChange={(event) => setRussian(event.target.value)} aria-label={t("taxonomy.russianLabelFor", { name: item.labels.en ?? item.key })} /></label>
        </div>
        <label className="required-control document-type-expert"><input type="checkbox" role="switch" className="switch" checked={expert} onChange={(event) => setExpert(event.target.checked)} aria-label={t("documentTypes.expertMode")} /><span>{t("documentTypes.expertMode")}</span></label>
        {expert ? <div className="type-key-line"><code>{item.key}</code>{item.builtin ? <span>{t("taxonomy.builtin")}</span> : null}</div> : null}
        <section className="taxonomy-subsection" aria-labelledby={`fields-${item.id}`}>
          <div className="taxonomy-subheading"><div><h4 id={`fields-${item.id}`}>{t("taxonomy.fields")}</h4><p>{t("taxonomy.fieldsDescription")}</p></div><span>{t("taxonomy.fieldCount", { count: fields.length })}</span></div>
          <div className="field-definition-list">
            {fields.map((field, index) => (
              <fieldset className="field-definition" key={`${item.id}-${index}`}>
                <legend>{t("taxonomy.fieldNumber", { number: index + 1 })}</legend>
                {expert ? (
                  <div className="field-definition-key">
                    <label className="field-label"><span>{t("taxonomy.fieldKey")}</span><input required pattern="[a-z][a-z0-9_]*" value={field.key} onChange={(event) => updateField(index, { key: event.target.value })} aria-label={t("taxonomy.fieldKeyNumber", { number: index + 1 })} /></label>
                    <div className="field-label"><span>{t("taxonomy.fieldKind")}</span><SelectDropdown value={field.kind} onChange={(kind) => updateField(index, { kind: kind as FieldKind, ...(kind === "text" ? {} : { issuer: false }), ...(kind === "identifier" ? {} : { validator: null }) })} aria-label={t("taxonomy.fieldKindNumber", { number: index + 1 })} options={fieldKinds.map((kind) => ({ value: kind, label: t(`taxonomy.kinds.${kind}`) }))} /></div>
                  </div>
                ) : null}
                <div className="taxonomy-label-grid">
                  <label className="field-label"><span>{t("taxonomy.englishLabel")}</span><input required value={field.labels.en ?? ""} onChange={(event) => updateLabel(index, "en", event.target.value)} aria-label={t("taxonomy.fieldEnglishNumber", { number: index + 1 })} /></label>
                  <label className="field-label"><span>{t("taxonomy.russianLabel")}</span><input required value={field.labels.ru ?? ""} onChange={(event) => updateLabel(index, "ru", event.target.value)} aria-label={t("taxonomy.fieldRussianNumber", { number: index + 1 })} /></label>
                </div>
                {field.kind === "identifier" ? (
                  <div className="field-label field-definition-validator">
                    <span>{t("taxonomy.fieldValidator")}</span>
                    <SelectDropdown
                      value={field.validator ?? ""}
                      onChange={(validator) => updateField(index, { validator: validator || null })}
                      aria-label={t("taxonomy.fieldValidatorNumber", { number: index + 1 })}
                      options={[
                        { value: "", label: t("taxonomy.validatorNone") },
                        ...KNOWN_VALIDATORS.map((validatorName) => ({ value: validatorName, label: t(`taxonomy.validators.${validatorName}`) })),
                      ]}
                    />
                  </div>
                ) : null}
                <div className="field-definition-footer">
                  <label className="required-control"><input type="checkbox" checked={field.required} onChange={(event) => updateField(index, { required: event.target.checked })} aria-label={t("taxonomy.fieldRequiredNumber", { number: index + 1 })} /><span>{t("taxonomy.required")}</span></label>
                  {field.kind === "text" ? <label className="required-control"><input type="checkbox" checked={field.issuer === true} onChange={(event) => updateField(index, { issuer: event.target.checked })} aria-label={t("taxonomy.fieldIssuerNumber", { number: index + 1 })} /><span>{t("taxonomy.issuer")}</span></label> : null}
                  <button type="button" className="quiet-button" onClick={() => setFields((current) => current.filter((_, position) => position !== index))}><X aria-hidden="true" />{t("taxonomy.removeField")}</button>
                </div>
              </fieldset>
            ))}
          </div>
          <button type="button" className="button button--secondary add-field-button" onClick={() => setFields((current) => [...current, { key: nextFieldKey(current), labels: { en: "", ru: "" }, kind: "text", required: false }])}><Plus aria-hidden="true" />{t("taxonomy.addField")}</button>
        </section>
        {error ? <div className="document-type-error" role="alert">{t(`errors.${error}`, { defaultValue: t("errors.taxonomy") })}</div> : null}
        <div className="type-save-row">
          <p role="status">{busy ? t("taxonomy.saving") : dirty ? t("documentTypes.unsaved") : t("documentTypes.saved")}</p>
          <button className="button button--primary" type="submit" disabled={busy || !dirty} aria-label={t("taxonomy.saveFor", { name: item.labels.en ?? item.key })}>{t("taxonomy.saveType")}</button>
        </div>
      </form>
    </div>
  );
}
