"""Check edited Rust lines without forcing unrelated historical reformatting."""

import difflib
import pathlib
import re
import subprocess
import sys


def git(*args):
    return subprocess.check_output(["git", *args], text=True, encoding="utf-8")


base = sys.argv[1] if len(sys.argv) > 1 else "HEAD^"
files = git("diff", "--name-only", "--diff-filter=ACM", "-z", base, "--", "*.rs")
failed = False
for filename in filter(None, files.split("\0")):
    diff = git("diff", "--no-ext-diff", "--unified=0", base, "--", filename)
    changed = set()
    for match in re.finditer(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", diff, re.M):
        start = int(match.group(1))
        count = int(match.group(2) or 1)
        changed.update(range(start, start + count) if count else (max(1, start), start + 1))
    original = pathlib.Path(filename).read_text(encoding="utf-8").splitlines()
    result = subprocess.run(
        ["rustfmt", "--edition", "2021", "--config", "skip_children=true",
         "--emit", "stdout", "--quiet", filename],
        text=True, encoding="utf-8", capture_output=True,
    )
    if result.returncode:
        print(result.stderr, file=sys.stderr)
        failed = True
        continue
    bad_lines = set()
    matcher = difflib.SequenceMatcher(None, original, result.stdout.splitlines(), autojunk=False)
    for kind, start, end, _, _ in matcher.get_opcodes():
        if kind == "equal":
            continue
        affected = set(range(start + 1, end + 1)) or {max(1, start), start + 1}
        bad_lines.update(changed & affected)
    if bad_lines:
        print(f"{filename}: format edited lines {', '.join(map(str, sorted(bad_lines)))}")
        failed = True
    else:
        print(f"{filename}: edited lines formatted; unrelated baseline differences preserved")
if not files:
    print("No Rust files changed.")
sys.exit(1 if failed else 0)
