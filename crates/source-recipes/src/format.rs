//! Display text for quantities, ingredients and yields.
//!
//! Quantities are stored as decimals and shown as fractions: halves, thirds,
//! quarters and eighths (`1.5` -> `1 1/2`). An ingredient's text is rebuilt
//! from its fields in one fixed order (see [`ingredient`]), the order of the
//! imported recipe file.

/// A unit's display names (`recipes_units.display` / `plural`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitNames {
    pub one: String,
    pub many: String,
}

/// One ingredient's fields (a `recipes_ingredients` row), with units already
/// looked up.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ingredient {
    pub qty_min: Option<f64>,
    pub qty_max: Option<f64>,
    pub unit: Option<UnitNames>,
    pub extra_qty: Option<f64>,
    pub extra_unit: Option<UnitNames>,
    pub pkg_qty: Option<f64>,
    /// The package size's unit code: `inch` shows as `"`.
    pub pkg_unit: Option<(String, UnitNames)>,
    pub name: String,
    pub preparation: Option<String>,
    pub note: Option<String>,
    pub scale_basis: Option<String>,
    pub is_optional: bool,
    pub is_divided: bool,
    pub to_taste: bool,
}

/// `1.5` -> `"1 1/2"`, `0.333..` -> `"1/3"`. Values that are not a whole
/// number of halves, thirds, quarters or eighths show as a short decimal.
pub fn qty(q: f64) -> String {
    let whole = (q + 1e-9).floor();
    let frac = q - whole;
    let whole = whole as i64;
    if frac.abs() < 1e-6 {
        return whole.to_string();
    }
    for den in [2_i64, 3, 4, 8] {
        let n = (frac * den as f64).round();
        if (frac * den as f64 - n).abs() < 0.01 && n > 0.0 && (n as i64) < den {
            let f = format!("{}/{den}", n as i64);
            return if whole > 0 { format!("{whole} {f}") } else { f };
        }
    }
    let s = format!("{q:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// [`qty`] with fraction characters, for the quantity column (D44):
/// `1.5` -> `"1½"`, `0.333..` -> `"⅓"`. Other values as [`qty`].
pub fn qty_glyph(q: f64) -> String {
    let plain = qty(q);
    let (whole, frac) = match plain.split_once(' ') {
        Some((w, f)) => (w, f),
        None if plain.contains('/') => ("", plain.as_str()),
        None => return plain,
    };
    let glyph = match frac {
        "1/2" => "½",
        "1/3" => "⅓",
        "2/3" => "⅔",
        "1/4" => "¼",
        "3/4" => "¾",
        "1/8" => "⅛",
        "3/8" => "⅜",
        "5/8" => "⅝",
        "7/8" => "⅞",
        _ => return plain,
    };
    format!("{whole}{glyph}")
}

fn unit_for(u: &UnitNames, amount: Option<f64>) -> &str {
    if amount.is_some_and(|a| a > 1.0) {
        &u.many
    } else {
        &u.one
    }
}

/// The ingredient as one line, e.g. `1 (15 oz) can kidney beans, rinsed and
/// drained` or `1/2 cup + 2 Tbsp olive oil (divided)`. The unit is plural
/// when the (upper) quantity is more than 1.
pub fn ingredient(i: &Ingredient) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(lo) = i.qty_min {
        let mut s = qty(lo);
        if let Some(hi) = i.qty_max {
            s.push('-');
            s.push_str(&qty(hi));
        }
        parts.push(s);
    }
    if let (Some(p), Some((code, names))) = (i.pkg_qty, &i.pkg_unit) {
        let u = if code == "inch" {
            "\"".to_owned()
        } else {
            format!(" {}", names.one)
        };
        parts.push(format!("({}{u})", qty(p)));
    }
    if let Some(u) = &i.unit {
        parts.push(unit_for(u, i.qty_max.or(i.qty_min)).to_owned());
    }
    if let (Some(x), Some(u)) = (i.extra_qty, &i.extra_unit) {
        parts.push(format!("+ {} {}", qty(x), unit_for(u, Some(x))));
    }
    parts.push(i.name.clone());
    if let Some(b) = &i.scale_basis {
        parts.push(b.clone());
    }
    let mut s = parts.join(" ");
    if let Some(p) = &i.preparation {
        s.push_str(", ");
        s.push_str(p);
    }
    if let Some(n) = &i.note {
        s.push_str(&format!(" ({n})"));
    }
    if i.is_optional {
        s.push_str(" (optional)");
    }
    if i.is_divided {
        s.push_str(" (divided)");
    }
    if i.to_taste {
        s.push_str(" (to taste)");
    }
    s
}

/// An ingredient split for a two-column list (D44): the amount
/// (`"1 (15 oz) can"`, `"½ cup + 2 Tbsp"`, `""` for none), the thing
/// (`"kidney beans"`) and the details (`"rinsed and drained, optional"`, `""`
/// for none). Quantities use fraction characters ([`qty_glyph`]).
pub fn ingredient_parts(i: &Ingredient) -> (String, String, String) {
    let mut amount: Vec<String> = Vec::new();
    if let Some(lo) = i.qty_min {
        let mut s = qty_glyph(lo);
        if let Some(hi) = i.qty_max {
            s.push('–');
            s.push_str(&qty_glyph(hi));
        }
        amount.push(s);
    }
    if let (Some(p), Some((code, names))) = (i.pkg_qty, &i.pkg_unit) {
        let u = if code == "inch" {
            "\"".to_owned()
        } else {
            format!(" {}", names.one)
        };
        amount.push(format!("({}{u})", qty_glyph(p)));
    }
    if let Some(u) = &i.unit {
        amount.push(unit_for(u, i.qty_max.or(i.qty_min)).to_owned());
    }
    if let (Some(x), Some(u)) = (i.extra_qty, &i.extra_unit) {
        amount.push(format!("+ {} {}", qty_glyph(x), unit_for(u, Some(x))));
    }
    let mut thing = i.name.clone();
    if let Some(b) = &i.scale_basis {
        thing.push(' ');
        thing.push_str(b);
    }
    let mut details: Vec<String> = Vec::new();
    details.extend(i.preparation.clone());
    details.extend(i.note.clone());
    for (on, word) in [
        (i.is_optional, "optional"),
        (i.is_divided, "divided"),
        (i.to_taste, "to taste"),
    ] {
        if on {
            details.push(word.to_owned());
        }
    }
    (amount.join(" "), thing, details.join(", "))
}

/// A time in minutes, or a range: `"15 min"`, `"15–20 min"`, `"1 hr"`,
/// `"1 hr 30 min"`.
pub fn minutes_text(min: i64, max: Option<i64>) -> String {
    fn one(m: i64) -> (String, bool) {
        if m < 60 {
            (m.to_string(), false)
        } else if m % 60 == 0 {
            (format!("{} hr", m / 60), true)
        } else {
            (format!("{} hr {} min", m / 60, m % 60), true)
        }
    }
    let (lo, lo_hr) = one(min);
    match max.filter(|&m| m > min) {
        Some(hi) if hi < 60 => format!("{lo}–{hi} min"),
        Some(hi) => {
            let lo = if lo_hr { lo } else { format!("{lo} min") };
            format!("{lo}–{}", one(hi).0)
        }
        None if lo_hr => lo,
        None => format!("{lo} min"),
    }
}

/// A short total time for a summary strip: `"55m"`, `"2h"`, `"1h 40m"`.
pub fn minutes_short(m: i64) -> String {
    match (m / 60, m % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// `Serves 4-6`, `Makes 3 dozen`, `Makes 12`; `None` without a yield.
pub fn yield_text(min: Option<f64>, max: Option<f64>, unit: Option<&str>) -> Option<String> {
    let min = min?;
    let mut n = qty(min);
    if let Some(max) = max {
        n.push('-');
        n.push_str(&qty(max));
    }
    Some(match unit {
        Some("servings") => format!("Serves {n}"),
        Some(u) => format!("Makes {n} {u}"),
        None => format!("Makes {n}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities_show_as_fractions() {
        assert_eq!(qty(1.0), "1");
        assert_eq!(qty(0.5), "1/2");
        assert_eq!(qty(2.5), "2 1/2");
        assert_eq!(qty(1.0 / 3.0), "1/3");
        assert_eq!(qty(2.0 / 3.0), "2/3");
        assert_eq!(qty(0.125), "1/8");
        assert_eq!(qty(1.75), "1 3/4");
        assert_eq!(qty(10.5), "10 1/2");
        assert_eq!(qty(0.3), "0.3");
    }

    #[test]
    fn quantities_with_fraction_characters() {
        assert_eq!(qty_glyph(1.0), "1");
        assert_eq!(qty_glyph(0.5), "½");
        assert_eq!(qty_glyph(2.5), "2½");
        assert_eq!(qty_glyph(1.0 / 3.0), "⅓");
        assert_eq!(qty_glyph(1.75), "1¾");
        assert_eq!(qty_glyph(0.375), "⅜");
        assert_eq!(qty_glyph(0.3), "0.3");
    }

    fn unit(one: &str, many: &str) -> Option<UnitNames> {
        Some(UnitNames {
            one: one.into(),
            many: many.into(),
        })
    }

    #[test]
    fn ingredients_split_into_amount_thing_and_details() {
        let beans = Ingredient {
            qty_min: Some(1.0),
            pkg_qty: Some(15.0),
            pkg_unit: Some(("oz".into(), unit("oz", "oz").unwrap())),
            unit: unit("can", "cans"),
            name: "kidney beans".into(),
            preparation: Some("rinsed and drained".into()),
            is_optional: true,
            ..Default::default()
        };
        assert_eq!(
            ingredient_parts(&beans),
            (
                "1 (15 oz) can".into(),
                "kidney beans".into(),
                "rinsed and drained, optional".into()
            )
        );
        let oil = Ingredient {
            qty_min: Some(0.5),
            unit: unit("cup", "cups"),
            extra_qty: Some(2.0),
            extra_unit: unit("Tbsp", "Tbsp"),
            name: "olive oil".into(),
            is_divided: true,
            ..Default::default()
        };
        assert_eq!(
            ingredient_parts(&oil),
            (
                "½ cup + 2 Tbsp".into(),
                "olive oil".into(),
                "divided".into()
            )
        );
        let salt = Ingredient {
            name: "salt and pepper".into(),
            ..Default::default()
        };
        assert_eq!(
            ingredient_parts(&salt),
            ("".into(), "salt and pepper".into(), "".into())
        );
        let range = Ingredient {
            qty_min: Some(4.0),
            qty_max: Some(6.0),
            unit: unit("cup", "cups"),
            name: "broth".into(),
            ..Default::default()
        };
        assert_eq!(ingredient_parts(&range).0, "4–6 cups");
    }

    #[test]
    fn times() {
        assert_eq!(minutes_text(15, None), "15 min");
        assert_eq!(minutes_text(15, Some(20)), "15–20 min");
        assert_eq!(minutes_text(60, None), "1 hr");
        assert_eq!(minutes_text(90, None), "1 hr 30 min");
        assert_eq!(minutes_text(45, Some(60)), "45 min–1 hr");
        assert_eq!(minutes_text(60, Some(120)), "1 hr–2 hr");
        assert_eq!(minutes_text(20, Some(20)), "20 min");
        assert_eq!(minutes_short(55), "55m");
        assert_eq!(minutes_short(120), "2h");
        assert_eq!(minutes_short(100), "1h 40m");
    }

    #[test]
    fn yields() {
        assert_eq!(
            yield_text(Some(4.0), Some(6.0), Some("servings")).unwrap(),
            "Serves 4-6"
        );
        assert_eq!(
            yield_text(Some(3.0), None, Some("dozen")).unwrap(),
            "Makes 3 dozen"
        );
        assert_eq!(yield_text(Some(12.0), None, None).unwrap(), "Makes 12");
        assert_eq!(yield_text(None, None, None), None);
    }
}
