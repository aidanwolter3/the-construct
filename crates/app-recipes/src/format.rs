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
