//! ICAO Doc 9303 machine-readable zone generation.

/// ICAO Doc 9303 Part 3 §4.9 check digit: weights 7,3,1 repeating.
pub fn icao_check(field: &str) -> u32 {
    const WEIGHTS: [u32; 3] = [7, 3, 1];
    field
        .bytes()
        .enumerate()
        .map(|(i, b)| {
            let v = match b {
                b'0'..=b'9' => u32::from(b - b'0'),
                b'A'..=b'Z' => u32::from(b - b'A') + 10,
                b'<' => 0,
                other => panic!("character {:?} is not allowed in an MRZ", other as char),
            };
            v * WEIGHTS[i % 3]
        })
        .sum::<u32>()
        % 10
}

/// The two TD3 machine-readable lines printed on `passport_td3.png`
/// (44 characters each). With `bad_check`, exactly one check digit is wrong.
pub fn passport_mrz(bad_check: bool) -> [String; 2] {
    let line1 = format!("P<UTOTESTOV<<IVAN{:<<27}", "");

    let doc_num = "TS1234567";
    let doc_check = if bad_check {
        (icao_check(doc_num) + 1) % 10
    } else {
        icao_check(doc_num)
    };
    let nationality = "UTO";
    let birth = "850312";
    let birth_check = icao_check(birth);
    let sex = "M";
    let expiry = "320901";
    let expiry_check = icao_check(expiry);
    let personal_num = "2001234567890<";
    let personal_check = icao_check(personal_num);

    let prefix = format!(
        "{doc_num}{doc_check}{nationality}{birth}{birth_check}{sex}{expiry}{expiry_check}{personal_num}{personal_check}"
    );
    // Composite check over positions 0..10, 13..20, 21..43 of line 2.
    let composite_input = format!("{}{}{}", &prefix[0..10], &prefix[13..20], &prefix[21..43]);
    let composite_check = icao_check(&composite_input);

    let line2 = format!("{prefix}{composite_check}");
    [line1, line2]
}

/// The three TD1 lines (30 characters each) printed on `id_card_td1.png`.
pub fn id_card_mrz() -> [String; 3] {
    let doc_num = "TS7654321";
    let doc_check = icao_check(doc_num);
    let line1 = format!("I<UTO{doc_num}{doc_check}{:<<15}", "");

    let birth = "900704";
    let birth_check = icao_check(birth);
    let sex = "F";
    let expiry = "310420";
    let expiry_check = icao_check(expiry);
    let nationality = "UTO";
    let opt_data = "<<<<<<<<<<<";

    let partial_l2 =
        format!("{birth}{birth_check}{sex}{expiry}{expiry_check}{nationality}{opt_data}");
    // Composite check over line1[5..30] + line2[0..7] + line2[8..15] + line2[18..29]
    let composite_input = format!(
        "{}{}{}{}",
        &line1[5..30],
        &partial_l2[0..7],
        &partial_l2[8..15],
        &partial_l2[18..29]
    );
    let composite_check = icao_check(&composite_input);
    let line2 = format!("{partial_l2}{composite_check}");

    let line3 = format!("TESTOVA<<ANNA{:<<17}", "");
    [line1, line2, line3]
}
