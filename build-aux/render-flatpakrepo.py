#!/usr/bin/env python3
import argparse
from pathlib import Path

parser = argparse.ArgumentParser(description="Render a Flatpak repo descriptor.")
parser.add_argument("--template", required=True)
parser.add_argument("--output", required=True)
parser.add_argument("--repo-url", required=True)
parser.add_argument("--gpg-key", required=True)
parser.add_argument("--no-gpg-verify", choices=("true", "false"), required=True)
args = parser.parse_args()

text = Path(args.template).read_text()
text = text.replace("@REPO_URL@", args.repo_url)
text = text.replace("@GPG_KEY@", args.gpg_key)
text = text.replace("@NO_GPG_VERIFY@", args.no_gpg_verify)
Path(args.output).write_text(text)
