#!/usr/bin/env python3
"""Small stdlib-only CLI for FlashPaste API automation."""

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request


def get_config():
    config_home = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    config_path = os.path.join(config_home, "flashpaste", "config.toml")
    file_cfg = {}
    if os.path.isfile(config_path):
        try:
            import tomllib
            with open(config_path, "rb") as f:
                file_cfg = tomllib.load(f)
        except Exception:
            pass
    url = os.environ.get("FLASHPASTE_URL") or file_cfg.get("url") or file_cfg.get("FLASHPASTE_URL") or "https://flashpaste.example.workers.dev"
    token = os.environ.get("FLASHPASTE_TOKEN") or file_cfg.get("token") or file_cfg.get("FLASHPASTE_TOKEN")
    return url.rstrip("/"), token


def request(path, method="GET", payload=None, raw=False):
    base, token = get_config()
    headers = {"Accept": "application/json", "User-Agent": "flashpaste-cli/1.0"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = None
    if payload is not None:
        data = json.dumps(payload, ensure_ascii=False).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            body = response.read()
    except urllib.error.HTTPError as error:
        body = error.read()
        try:
            detail = json.loads(body).get("error", body.decode(errors="replace"))
        except (ValueError, AttributeError):
            detail = body.decode(errors="replace")
        raise SystemExit(f"HTTP {error.code}: {detail}")
    return body if raw else json.loads(body or b"null")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("list", help="list dashboard items")
    get = sub.add_parser("get", help="read content body")
    get.add_argument("id")
    get.add_argument("--preview", action="store_true", help="do not consume burn content (creator/super only)")
    create = sub.add_parser("create", help="create content from stdin or a file")
    create.add_argument("--title")
    create.add_argument("--type", choices=("text", "markdown", "html"))
    create.add_argument("--visibility", choices=("public", "token"), default="public")
    create.add_argument("--ttl", choices=("1h", "6h", "1d", "2d", "burn"), default="1h")
    create.add_argument("--file", help="UTF-8 file to share; otherwise read stdin")
    create.add_argument("--grant", action="append", default=[], metavar="TOKEN_NAME")
    delete = sub.add_parser("delete", help="delete content you own or manage as super")
    delete.add_argument("id")
    lookup = sub.add_parser("lookup-token", help="resolve a token name to its ID")
    lookup.add_argument("name")
    download = sub.add_parser("download", help="download original content bytes")
    download.add_argument("id")
    download.add_argument("--output", default="-")
    args = parser.parse_args()

    if args.command == "list":
        result = request("/api/dashboard")
        print(json.dumps(result, ensure_ascii=False, indent=2))
    elif args.command == "lookup-token":
        print(json.dumps(request("/api/tokens/lookup?name=" + urllib.parse.quote(args.name, safe="")), ensure_ascii=False))
    elif args.command == "delete":
        print(json.dumps(request("/api/contents/" + urllib.parse.quote(args.id, safe=""), "DELETE"), ensure_ascii=False))
    elif args.command == "get":
        suffix = "?consume=0" if args.preview else ""
        result = request("/api/contents/" + urllib.parse.quote(args.id, safe="") + "/body" + suffix)
        sys.stdout.write(result["content"])
    elif args.command == "download":
        body = request("/d/" + urllib.parse.quote(args.id, safe=""), raw=True)
        if args.output == "-":
            sys.stdout.buffer.write(body)
        else:
            with open(args.output, "wb") as output:
                output.write(body)
    elif args.command == "create":
        _, token = get_config()
        if not token:
            raise SystemExit("Set FLASHPASTE_TOKEN in environment or in ~/.config/flashpaste/config.toml before creating content")
        if args.file:
            with open(args.file, encoding="utf-8", newline="") as source:
                content = source.read()
            inferred = "html" if args.file.lower().endswith((".html", ".htm")) else "markdown" if args.file.lower().endswith(".md") else "text"
            content_type = args.type or inferred
        else:
            content = sys.stdin.read()
            content_type = args.type or "text"
        title = (args.title or "").strip() or None
        grants = [request("/api/tokens/lookup?name=" + urllib.parse.quote(name, safe=""))["id"] for name in args.grant]
        ttls = {"1h": 3600, "6h": 21600, "1d": 86400, "2d": 172800, "burn": 604800}
        payload = {"title": title, "content_type": content_type, "content": content,
                   "visibility": args.visibility, "ttl": ttls[args.ttl],
                   "burn_after_read": args.ttl == "burn", "allowed_tokens": grants}
        print(json.dumps(request("/api/contents", "POST", payload), ensure_ascii=False))


if __name__ == "__main__":
    main()
