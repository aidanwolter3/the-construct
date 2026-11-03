#!/usr/bin/env python3
"""Turn the family recipe PDF into the recipes adapter's import file.

    pdftotext -layout All_Recipes_Formatted.pdf recipes.txt
    python3 -I scripts/recipes_from_pdf.py recipes.txt crates/app-recipes/data/import-0001.json

The PDF is already normalized (one format for every recipe), so this is a plain
line parser. Each ingredient line is split into the fields of
`recipes_ingredients` (quantity range, package size, unit, "+ extra", name,
preparation, note, flags, alternatives), and the original line is kept as
`line`: the adapter's tests check that formatting the fields gives it back.

The import file is frozen once deployed (migration 2 of the recipes adapter
reads it). Re-running this is only for a fresh import file.
"""

import json
import re
import sys

CATEGORIES = [
    "Appetizers/Sides/Salads",
    "Main Dishes (Meat/Casseroles)",
    "Side Dishes (Rice/Grains)",
    "Desserts",
    "Breakfast",
    "Baking (Breads/Crusts/Frosting)",
    "Snacks/Confections",
    "Drinks",
]

# Words that are a unit when they follow a quantity (and more text follows).
UNIT_WORDS = {
    "tsp": "tsp", "Tbsp": "tbsp", "cup": "cup", "cups": "cup", "oz": "oz",
    "lb": "lb", "pt": "pt", "qt": "qt", "gal": "gal", "ml": "ml",
    "pinch": "pinch", "dash": "dash", "splash": "splash",
    "can": "can", "cans": "can", "pkg": "pkg", "box": "box", "boxes": "box",
    "bottle": "bottle", "bottles": "bottle", "stick": "stick", "sticks": "stick",
    "square": "square", "squares": "square", "clove": "clove", "cloves": "clove",
    "bunch": "bunch", "bunches": "bunch", "head": "head", "slice": "slice",
    "slices": "slice", "sprig": "sprig", "stalk": "stalk", "stalks": "stalk",
    "inch": "inch", "container": "container",
}
# code -> (singular, plural); must match the recipes_units rows.
UNIT_DISPLAY = {
    "tsp": ("tsp", "tsp"), "tbsp": ("Tbsp", "Tbsp"), "cup": ("cup", "cups"),
    "fl_oz": ("fl oz", "fl oz"), "oz": ("oz", "oz"), "lb": ("lb", "lb"),
    "pt": ("pt", "pt"), "qt": ("qt", "qt"), "gal": ("gal", "gal"),
    "ml": ("ml", "ml"), "l": ("l", "l"), "g": ("g", "g"),
    "pinch": ("pinch", "pinches"), "dash": ("dash", "dashes"),
    "splash": ("splash", "splashes"), "can": ("can", "cans"),
    "pkg": ("pkg", "pkg"), "box": ("box", "boxes"),
    "bottle": ("bottle", "bottles"), "stick": ("stick", "sticks"),
    "square": ("square", "squares"), "clove": ("clove", "cloves"),
    "bunch": ("bunch", "bunches"), "head": ("head", "heads"),
    "slice": ("slice", "slices"), "sprig": ("sprig", "sprigs"),
    "stalk": ("stalk", "stalks"), "inch": ("inch", "inches"),
    "container": ("container", "containers"),
}

NUM = r"\d+ \d+/\d+|\d+/\d+|\d+(?:\.\d+)?"
QTY = re.compile(rf"^({NUM})(?:-({NUM}))? (.*)$")
PKG = re.compile(rf'^\(({NUM})(?: (oz|ml)|("))\) (.*)$')
EXTRA = re.compile(rf"^\+ ({NUM}) (\S+) (.*)$")
TRAILING_PAREN = re.compile(r"^(.*\S) \(([^()]*)\)$")
NOTE_BEFORE_PREP = re.compile(r"^(.*?) \(([^()]*)\), (.*)$")
PER = re.compile(r"^(.*) (per (?:pan|serving))$")
# Commas that belong to the name, not to a preparation.
NAME_COMMAS = ["boneless, skinless"]

BULLET = re.compile(r"^\s*-​?\s+(.*)$")
STEP = re.compile(r"^\s*(\d+)\.​?\s+(.*)$")


def num(s):
    s = s.strip()
    if " " in s:
        whole, frac = s.split(" ")
        return int(whole) + num(frac)
    if "/" in s:
        a, b = s.split("/")
        return int(a) / int(b)
    return float(s)


def fmt_qty(q):
    """1.5 -> '1 1/2'; eighths and thirds (the same rule as the adapter)."""
    whole = int(q + 1e-9)
    frac = q - whole
    if abs(frac) < 1e-6:
        return str(whole)
    for den in (2, 3, 4, 8):
        n = round(frac * den)
        if abs(frac * den - n) < 0.01 and 0 < n < den:
            f = f"{n}/{den}"
            return f"{whole} {f}" if whole else f
    raise ValueError(f"quantity {q} is not in eighths or thirds")


def unit_text(code, upper):
    one, many = UNIT_DISPLAY[code]
    return many if upper is not None and upper > 1 else one


def parse_one(text):
    """One ingredient (no ' or ' alternative) -> dict of fields."""
    ing = {
        "qty_min": None, "qty_max": None, "unit": None,
        "extra_qty": None, "extra_unit": None,
        "pkg_qty": None, "pkg_unit": None,
        "name": None, "preparation": None, "note": None, "scale_basis": None,
        "is_optional": False, "is_divided": False, "to_taste": False,
    }
    rest = text
    m = QTY.match(rest)
    if m:
        ing["qty_min"] = num(m.group(1))
        ing["qty_max"] = num(m.group(2)) if m.group(2) else None
        rest = m.group(3)
        m = PKG.match(rest)
        if m:
            ing["pkg_qty"] = num(m.group(1))
            ing["pkg_unit"] = m.group(2) or "inch"
            rest = m.group(4)
        word, _, after = rest.partition(" ")
        if word in UNIT_WORDS and after:
            ing["unit"] = UNIT_WORDS[word]
            rest = after
            m = EXTRA.match(rest)
            if m:
                ing["extra_qty"] = num(m.group(1))
                ing["extra_unit"] = UNIT_WORDS[m.group(2)]
                rest = m.group(3)
    notes = []
    # "2 cups (12 oz) semisweet chocolate chips": an equivalent after the unit.
    m = re.match(r"^\(([^()]*)\) (.*)$", rest)
    if m:
        notes.append(m.group(1))
        rest = m.group(2)
    flags = []
    while True:
        m = TRAILING_PAREN.match(rest)
        if not m:
            break
        rest, inner = m.group(1), m.group(2)
        flags.insert(0, inner)
    for inner in flags:
        if inner == "optional":
            ing["is_optional"] = True
        elif inner == "divided":
            ing["is_divided"] = True
        elif inner == "to taste":
            ing["to_taste"] = True
        else:
            notes.append(inner)
    m = NOTE_BEFORE_PREP.match(rest)
    if m and "(" not in m.group(1):
        rest = f"{m.group(1)}, {m.group(3)}"
        notes.insert(0, m.group(2))
    protected = rest
    for i, phrase in enumerate(NAME_COMMAS):
        protected = protected.replace(phrase, f"\0{i}\0")
    name, sep, prep = protected.partition(", ")
    for i, phrase in enumerate(NAME_COMMAS):
        name = name.replace(f"\0{i}\0", phrase)
        prep = prep.replace(f"\0{i}\0", phrase)
    m = PER.match(name)
    if m:
        name, ing["scale_basis"] = m.group(1), m.group(2)
    ing["name"] = name
    ing["preparation"] = prep if sep else None
    if len(notes) > 1:
        raise ValueError(f"two notes in {text!r}: {notes}")
    ing["note"] = notes[0] if notes else None
    return ing


def format_one(ing):
    """The display text of one ingredient (mirrors the adapter's formatter)."""
    parts = []
    lo, hi = ing["qty_min"], ing["qty_max"]
    if lo is not None:
        parts.append(fmt_qty(lo) + (f"-{fmt_qty(hi)}" if hi is not None else ""))
    if ing["pkg_qty"] is not None:
        u = '"' if ing["pkg_unit"] == "inch" else " " + unit_text(ing["pkg_unit"], 1)
        parts.append(f"({fmt_qty(ing['pkg_qty'])}{u})")
    if ing["unit"]:
        parts.append(unit_text(ing["unit"], hi if hi is not None else lo))
    if ing["extra_qty"] is not None:
        parts += ["+", fmt_qty(ing["extra_qty"]), unit_text(ing["extra_unit"], ing["extra_qty"])]
    parts.append(ing["name"])
    if ing["scale_basis"]:
        parts.append(ing["scale_basis"])
    s = " ".join(parts)
    if ing["preparation"]:
        s += ", " + ing["preparation"]
    if ing["note"]:
        s += f" ({ing['note']})"
    if ing["is_optional"]:
        s += " (optional)"
    if ing["is_divided"]:
        s += " (divided)"
    if ing["to_taste"]:
        s += " (to taste)"
    return s


# Lines whose alternative needs a fuller primary than the text gives.
PRIMARY_NAMES = {"1 large or 2 small lemons, peeled and seeded": "large lemon"}


def parse_ingredient(line):
    head, sep, alt = line.partition(" or ")
    if sep and re.match(r"\d", alt):
        ing = parse_one(head)
        if line in PRIMARY_NAMES:
            ing["name"] = PRIMARY_NAMES[line]
        ing["alternatives"] = [parse_one(alt)]
    else:
        ing = parse_one(line)
        ing["alternatives"] = []
    ing["line"] = line
    return ing


def format_ingredient(ing):
    return " or ".join([format_one(ing)] + [format_one(a) for a in ing["alternatives"]])


TEMP = re.compile(r"(\d+)°F")
TIME = re.compile(r"(\d+(?: 1/2)?)(?:-(\d+(?: 1/2)?))? (min|hr|hrs)\b")


def step_fields(text):
    temp = TEMP.search(text)
    time = TIME.search(text)
    out = {"text": text, "temp_f": int(temp.group(1)) if temp else None,
           "time_min": None, "time_max": None}
    if time:
        mult = 60 if time.group(3).startswith("hr") else 1
        out["time_min"] = round(num(time.group(1)) * mult)
        if time.group(2):
            out["time_max"] = round(num(time.group(2)) * mult)
    return out


def slugify(title):
    return re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")


YIELD = re.compile(rf"^(Serves|Makes) ({NUM})(?:-({NUM}))?(?: (.+))?$")


def parse_meta(line):
    parts = [p.strip() for p in line.split("·")]
    source = parts[-1].removeprefix("Source:").strip()
    yld = None
    if len(parts) == 2:
        m = YIELD.match(parts[0])
        if not m:
            raise ValueError(f"yield {parts[0]!r}")
        unit = "servings" if m.group(1) == "Serves" else m.group(4)
        yld = {"min": num(m.group(2)),
               "max": num(m.group(3)) if m.group(3) else None, "unit": unit}
    return source, yld


def parse(text):
    lines = [ln.replace("\f", "").rstrip() for ln in text.split("\n")]
    recipes, category, cur, section = [], None, None, None
    i = 0
    while i < len(lines):
        raw = lines[i]
        s = raw.strip()
        nxt = lines[i + 1].strip() if i + 1 < len(lines) else ""
        i += 1
        if not s:
            continue
        if s in CATEGORIES:
            category, cur = s, None
            continue
        if nxt.startswith("Source:") or re.match(r"^(Serves|Makes) .*· Source:", nxt):
            source, yld = parse_meta(nxt)
            i += 1
            cur = {"slug": slugify(s), "title": s, "category": category,
                   "source": source, "yield": yld, "ingredient_groups": [],
                   "instruction_groups": [], "notes": []}
            recipes.append(cur)
            section = None
            continue
        if cur is None:
            raise ValueError(f"line outside a recipe: {raw!r}")
        if s in ("Ingredients", "Instructions", "Notes"):
            section = s
            continue
        b, st = BULLET.match(raw), STEP.match(raw)
        if section == "Ingredients":
            if b:
                if not cur["ingredient_groups"]:
                    cur["ingredient_groups"].append({"name": None, "ingredients": []})
                cur["ingredient_groups"][-1]["ingredients"].append(parse_ingredient(b.group(1).strip()))
            else:
                cur["ingredient_groups"].append({"name": s, "ingredients": []})
        elif section == "Instructions":
            if st:
                if not cur["instruction_groups"]:
                    cur["instruction_groups"].append({"name": None, "steps": []})
                cur["instruction_groups"][-1]["steps"].append(st.group(2).strip())
            elif len(raw) - len(raw.lstrip()) >= 5:
                steps = cur["instruction_groups"][-1]["steps"]
                steps[-1] += " " + s
            else:
                cur["instruction_groups"].append({"name": s, "steps": []})
        elif section == "Notes":
            if b:
                cur["notes"].append(b.group(1).strip())
            else:
                cur["notes"][-1] += " " + s
        else:
            raise ValueError(f"line outside a section in {cur['title']}: {raw!r}")
    for r in recipes:
        for g in r["instruction_groups"]:
            g["steps"] = [step_fields(t) for t in g["steps"]]
    return recipes


def main():
    src, out = sys.argv[1], sys.argv[2]
    with open(src, encoding="utf-8") as f:
        recipes = parse(f.read())
    slugs = [r["slug"] for r in recipes]
    assert len(set(slugs)) == len(slugs), "duplicate slugs"
    bad = 0
    for r in recipes:
        for g in r["ingredient_groups"]:
            for ing in g["ingredients"]:
                got = format_ingredient(ing)
                if got != ing["line"] and ing["line"] not in PRIMARY_NAMES:
                    bad += 1
                    print(f"{r['title']}: {ing['line']!r} -> {got!r}", file=sys.stderr)
    data = {
        "categories": [{"slug": slugify(c), "name": c} for c in CATEGORIES],
        "recipes": recipes,
    }
    with open(out, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=1, ensure_ascii=False)
        f.write("\n")
    counts = {c: sum(r["category"] == c for r in recipes) for c in CATEGORIES}
    print(f"{len(recipes)} recipes, {bad} ingredient lines that format differently", file=sys.stderr)
    for c, n in counts.items():
        print(f"  {n:3} {c}", file=sys.stderr)


if __name__ == "__main__":
    main()
