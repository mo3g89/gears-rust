#!/usr/bin/env python3
"""DESIGN.md section 3.13 documents only keys the config structs really have.

Every gear's and every product plugin's config struct is `serde(deny_unknown_fields)`, so a config written
faithfully from a wrong section 3.13 is a hard startup failure, not a warning.
Two such defects survived every earlier pass: `log_follow_idle_seconds`
documented at the gear's top level when it is a field of the nested
`ArgoExecutorConfig`, and `argo.bundle_auth`, a key that no longer exists.
A grep for "does this identifier appear anywhere" passes on both, so this check
is nesting-aware: it resolves each dotted key from the gear's root struct,
following each field's declared type into the nested struct.

Usage: check_design_config_keys.py [DESIGN.md path]
The optional path lets a test point the check at a doctored copy.
"""
import re
import sys
from pathlib import Path

GEAR = Path(__file__).resolve().parents[3]  # gears/qa-platform
DESIGN = GEAR / "docs" / "DESIGN.md"
ROOTS = {  # section 3.13 "Gear" column -> (source file, root struct)
    "runs": ("qa-runs/qa-runs/src/config.rs", "QaRunsConfig"),
    "environments": ("qa-environments/qa-environments/src/config.rs", "QaEnvironmentsConfig"),
    "catalog": ("qa-catalog/qa-catalog/src/config.rs", "QaCatalogConfig"),
    "insights": ("qa-insights/qa-insights/src/config.rs", "QaInsightsConfig"),
    # The product plugins are gears with their own `config:` block, and their
    # config structs are `deny_unknown_fields` exactly like the four gears'.
    "vhp-plugin": ("plugins/qa-vhp-product-plugin/src/gear.rs", "VhpProductPluginConfig"),
    "vhi-plugin": ("plugins/qa-vhi-product-plugin/src/gear.rs", "VhiProductPluginConfig"),
}


def parse_structs(src):
    """{struct name: {field name: type text}} for every struct in `src`."""
    structs = {}
    for m in re.finditer(r"^pub struct (\w+)\s*\{", src, re.M):
        depth, i = 1, m.end()
        while depth:
            depth += {"{": 1, "}": -1}.get(src[i], 0)
            i += 1
        body = src[m.end():i - 1]
        structs[m.group(1)] = dict(re.findall(r"^\s*pub (\w+):\s*([^,\n]+),", body, re.M))
    return structs


def section(text):
    start = text.index("### 3.13 Configuration")
    m = re.search(r"^#{3,4} ", text[start + 5:], re.M)
    return text[start:start + 5 + m.start()] if m else text[start:]


def rows(sec):
    """(gear, [keys]) per table row; `.leaf` continues the previous key's prefix."""
    out = []
    for line in sec.splitlines():
        cells = [c.strip() for c in line.strip().strip("|").split(" | ")] if line.startswith("| `") else None
        if not cells or len(cells) < 2:
            continue
        keys, prefix = [], ""
        for k in re.findall(r"`([^`]+)`", cells[0]):
            if k.startswith("."):
                k = prefix + k
            else:
                prefix = k.rsplit(".", 1)[0] if "." in k else ""
            keys.append(k)
        out.append((cells[1], keys))
    return out


def main():
    design = Path(sys.argv[1]) if len(sys.argv) > 1 else DESIGN
    problems = []
    parsed = {}
    for gear, (rel, root) in ROOTS.items():
        structs = parse_structs((GEAR / rel).read_text())
        if not structs.get(root):
            problems.append(f"could not read root struct {root} from {rel}")
        parsed[gear] = structs
    table = rows(section(design.read_text()))
    checked = 0
    for gear, keys in table:
        if gear not in ROOTS:
            problems.append(f"unknown gear column {gear!r} for keys {keys}")
            continue
        structs, cur_root = parsed[gear], ROOTS[gear][1]
        for key in keys:
            checked += 1
            cur, walked = cur_root, []
            for part in key.split("."):
                fields = structs.get(cur, {})
                if part not in fields:
                    where = [n for n, f in structs.items() if part in f]
                    for other in parsed.values():
                        where += [n for n, f in other.items() if part in f and n not in where]
                    hint = (f"; `{part}` is a field of {', '.join(sorted(set(where)))}, so it is documented at the wrong depth"
                            if where else "; no struct has a field of that name, so the key no longer exists")
                    at = ".".join(walked) or "the top level"
                    problems.append(f"[{gear}] `{key}`: {cur} (at {at}) has no field `{part}`{hint}")
                    break
                walked.append(part)
                nxt = re.sub(r"^Option<(.*)>$", r"\1", fields[part].strip())
                cur = nxt
    covered = {gear for gear, _ in table}
    for gear in ROOTS:
        if gear not in covered:
            problems.append(f"no section 3.13 row documents any `{gear}` key")
    if checked < 40:  # a parse that found almost nothing must not read as a pass
        problems.append(f"only {checked} keys parsed out of section 3.13; the table or the parser is broken")
    if problems:
        print("DESIGN.md 3.13 disagrees with the config structs:\n  " + "\n  ".join(problems))
        return 1
    print(f"ok: {checked} documented config keys resolve against the config structs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
