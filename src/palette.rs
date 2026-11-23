//! Localized labels and search terms for the GUI component palette.

use crate::library::CompInfo;

pub const DEFAULT_FAVORITES: &[&str] = &["R", "C", "L", "GND", "Vdc", "Vac", "Diode", "OpAmp", ".TR", ".AC"];

pub fn category_name(category: &str) -> &str {
    match category {
        "lumped components" => "基本部品",
        "sources" => "電源・信号源",
        "simulations" => "シミュレーション",
        "nonlinear components" => "非線形部品",
        "microelectronics" => "半導体・SPICE モデル",
        "digital components" => "デジタル部品",
        "RF components" => "高周波部品",
        "transmission lines" => "伝送線路",
        "magnetic cores" => "磁性体・巻線",
        "file components" => "ファイル・サブ回路",
        "SPICE / other" => "SPICE・その他",
        "verilog-a devices" => "Verilog-A デバイス",
        _ => category,
    }
}

pub fn caption(part: &CompInfo) -> &str {
    match part.typ.as_str() {
        ".TR" => "過渡解析",
        ".AC" => "AC 解析",
        ".DC" => "DC 動作点",
        "Vdc" => "直流電圧源",
        "Vac" => "交流電圧源",
        "Idc" => "直流電流源",
        "Iac" => "交流電流源",
        "R_SPICE" => "抵抗（SPICE）",
        "C_SPICE" => "コンデンサ（SPICE）",
        "L_SPICE" => "インダクタ（SPICE）",
        "DIODE_SPICE" => "ダイオード（SPICE）",
        "Eqn" => "数式",
        "NutmegEq" => "Nutmeg 式",
        "SpicePar" => "SPICE パラメータ",
        "SpiceInclude" => "SPICE ファイルの読み込み",
        "SpiceModel" => "SPICE モデル",
        _ if part.caption.is_empty() => &part.typ,
        _ => &part.caption,
    }
}

fn aliases(typ: &str) -> &'static str {
    match typ {
        "R" | "R_SPICE" => "抵抗器 れじすた resistor resistance",
        "potentiometer" => "可変抵抗 ボリューム variable resistor",
        "C" | "C_SPICE" | "CAPQ" => "容量 キャパシタ 静電容量 コンデンサー capacitor capacitance",
        "L" | "L_SPICE" | "INDQ" => "コイル リアクトル インダクター inductor inductance coil",
        "GND" => "接地 アース グランド グラウンド ground earth",
        "Vdc" => "電源 直流電源 電池 dc voltage source battery",
        "Vac" => "電源 交流電源 正弦波 ac voltage source sine",
        "Idc" => "電源 直流電源 dc current source",
        "Iac" => "電源 交流電源 ac current source",
        "Vpulse" | "Vrect" => "電源 パルス 矩形波 電圧源 pulse voltage source square wave",
        "Ipulse" | "Irect" => "電源 パルス 矩形波 電流源 pulse current source square wave",
        "VProbe" => "電圧計 プローブ voltage probe voltmeter",
        "IProbe" => "電流プローブ プローブ current probe ammeter",
        "OpAmp" => "演算増幅器 オペアンプ op amp opamp operational amplifier",
        "Diode" | "DIODE_SPICE" => "整流器 整流 diode rectifier",
        "BJT" | "_BJT" | "BJT_SPICE" | "NPN_SPICE" | "PNP_SPICE" => "バイポーラ トランジスター transistor bjt",
        "MOSFET" | "_MOSFET" | "MOS_SPICE" | "NMOS_SPICE" | "PMOS_SPICE" | "VDMOS" => "トランジスタ 電界効果 fet mosfet",
        "JFET" | "NJF_SPICE" | "PJF_SPICE" => "トランジスタ 電界効果 fet jfet",
        "Tr" | "sTr" => "変圧器 transformer",
        ".TR" => "過渡応答 時間応答 transient time domain",
        ".AC" => "周波数応答 交流解析 ボード線図 ac simulation frequency response",
        ".DC" => "直流解析 動作点 operating point dc simulation",
        ".SW" => "掃引 パラメータ sweep",
        _ => "",
    }
}

/// Fold full-width ASCII, case, and hiragana into the same search representation.
pub fn search_text(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap_or(c),
            '\u{3041}'..='\u{3096}' => char::from_u32(c as u32 + 0x60).unwrap_or(c),
            c => c,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn matches(part: &CompInfo, query: &str) -> bool {
    let fields = search_text(&format!(
        "{} {} {} {} {}",
        part.typ, part.caption, caption(part), part.description, aliases(&part.typ)
    ));
    query.split_whitespace().all(|word| {
        if word.len() == 1 && word.as_bytes()[0].is_ascii_alphanumeric() {
            fields.split_whitespace().any(|field| field == word)
        } else {
            fields.contains(word)
        }
    })
}

const LIBRARY_TERMS: &[(&[&str], &[&str])] = &[
    (&["ダイオード", "整流"], &["diode", "rectifier", "bridges"]),
    (&["トランジスタ"], &["transistor", "bjt", "fet"]),
    (&["オペアンプ", "演算増幅器"], &["opamp", "op amp", "operational amplifier"]),
    (&["比較器", "コンパレータ"], &["comparator"]),
    (&["発光ダイオード"], &["led", "light emitting diode"]),
    (&["電圧レギュレータ", "定電圧", "レギュレータ"], &["regulator"]),
    (&["水晶", "クリスタル"], &["crystal"]),
    (&["サーミスタ"], &["thermistor"]),
    (&["変圧器", "トランス"], &["transformer"]),
    (&["コンデンサ", "キャパシタ"], &["capacitor"]),
    (&["インダクタ", "コイル"], &["inductor"]),
    (&["抵抗", "抵抗器"], &["resistor", "resistance"]),
];

pub fn library_matches(library: &str, name: &str, description: &str, query: &str) -> bool {
    let fields = search_text(&format!("{library} {name} {description}"));
    query.split_whitespace().all(|word| {
        fields.contains(word)
            || LIBRARY_TERMS.iter().any(|(japanese, english)| {
                japanese.contains(&word) && english.iter().any(|term| fields.contains(term))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_japanese_english_and_full_width_type_ids() {
        let lib = crate::library::lib();
        let resistor = lib.info("R").unwrap();
        let capacitor = lib.info("C").unwrap();
        assert!(matches(resistor, &search_text("Ｒ")));
        assert!(!matches(capacitor, &search_text("Ｒ")));
        assert!(matches(resistor, &search_text("抵抗器 resistor")));
        assert!(matches(capacitor, &search_text("こんでんさ")));
        assert!(matches(lib.info(".TR").unwrap(), &search_text("過渡応答")));
        assert!(library_matches("OpAmps", "LM358", "dual amplifier", &search_text("ＬＭ３５８ オペアンプ")));
        assert!(!library_matches("Diodes", "1N4148", "switching diode", &search_text("オペアンプ")));
    }
}
