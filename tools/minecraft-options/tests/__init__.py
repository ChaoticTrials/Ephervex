"""Tests for the Minecraft options tooling.

    python3 -m unittest discover -s tools/minecraft-options

The tools sit one directory above and are imported by their module names, so
that directory is put on the import path for any way of starting a test.
"""

import sys
from pathlib import Path

TOOL_DIR = Path(__file__).resolve().parent.parent
if str(TOOL_DIR) not in sys.path:
    sys.path.insert(0, str(TOOL_DIR))
