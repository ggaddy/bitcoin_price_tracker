#!/usr/bin/env python3
"""Fail CI when the documented image advisory review expires or is missing."""
from datetime import date
from pathlib import Path
import re

config = (Path(__file__).resolve().parents[1] / '.grype.yaml').read_text()
match = re.search(r'^# Review by: (\d{4}-\d{2}-\d{2})$', config, re.MULTILINE)
if not match:
    raise SystemExit('Image advisory exceptions need a review date and SECURITY.md rationale')
deadline = date.fromisoformat(match[1])
if date.today() >= deadline:
    raise SystemExit(f'Image advisory review expired on {deadline}; recheck exposure and available updates')
print(f'Image advisory exceptions must be reviewed before {deadline}; see SECURITY.md')
