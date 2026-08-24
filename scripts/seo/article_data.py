#!/usr/bin/env python3
"""Strict frontmatter and article metadata support for the SEO toolchain.

This module deliberately implements only the small YAML-ish dialect used by
the article sources.  It is shared by ``build_articles.py``, ``pagedata.py``
and ``urlmap.py`` so that article kind, dates and ``noindex`` never drift
between the generator and the existing SEO checks.
"""

from __future__ import annotations

import ast
import dataclasses
import datetime as dt
import json
import pathlib
import re
import shlex
from typing import Any


TOP_FIELDS = frozenset({
    "title", "h1", "description", "slug", "cluster", "question", "intent",
    "published", "updated", "author", "reviewer", "schema_type", "links_out",
    "evidence", "faq", "noindex", "agent_prompt", "commands",
})
# ``evidence`` is optional.  The block existed to point at capture files that
# are no longer part of the repository, so an article without one is normal and
# renders no provenance section at all.  It stays in TOP_FIELDS and keeps its
# full schema validation, so an article that does carry one is still checked
# key-by-key and its sources still have to resolve.
REQUIRED_FIELDS = frozenset({
    "title", "h1", "description", "slug", "cluster", "question", "intent",
    "published", "author", "reviewer", "schema_type", "links_out",
    "faq",
})
STRING_FIELDS = frozenset({
    "title", "h1", "description", "slug", "cluster", "question", "intent",
    "author", "reviewer", "schema_type", "agent_prompt",
})
LIST_FIELDS = frozenset({"links_out", "evidence", "faq", "commands"})
MAP_FIELDS = frozenset({"evidence", "faq", "commands"})
MAP_KEYS = {
    "evidence": frozenset({"claim", "source"}),
    "faq": frozenset({"q", "a"}),
    "commands": frozenset({"cmd", "note"}),
}

TOP_KEY_RE = re.compile(r"^([A-Za-z][A-Za-z0-9_]*)\s*:\s*(.*?)\s*$")
NESTED_KEY_RE = re.compile(r"^ {4}([A-Za-z][A-Za-z0-9_]*)\s*:\s*(.*?)\s*$")
LIST_ITEM_RE = re.compile(r"^ {2}-\s*(.*?)\s*$")
SLUG_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
INT_RE = re.compile(r"^-?(?:0|[1-9]\d*)$")
COMMAND_TOKEN_RE = re.compile(r"^-{1,2}[A-Za-z][A-Za-z0-9-]*(?:=.*)?$")
# Verified against `engine/target/ci-test/xerj <cmd> --help` on 2026-08-22.
# The list previously allowed `serve` and `verify`, which are not subcommands
# (the binary answers `unknown argument`), while omitting `index` and `brain`,
# which are — so it rejected a page documenting `xerj brain ./notes` and waved
# through a command nobody can run. Check the binary before editing this.
XERJ_COMMANDS = frozenset({"index", "autoindex", "brain", "mcp", "help", "version"})
SHELL_PRIMITIVES = frozenset({
    "bash", "cd", "command", "curl", "env", "export", "git", "sh", "until", "wget",
})


class FrontmatterError(ValueError):
    """A parse or schema error with a source path and 1-based line number."""

    def __init__(self, path: pathlib.Path | str, line: int, message: str):
        self.path = str(path)
        self.line = line
        self.message = message
        super().__init__(f"{self.path}:{line}: {message}")


@dataclasses.dataclass(frozen=True)
class Article:
    path: pathlib.Path
    category: str
    title: str
    h1: str
    description: str
    slug: str
    cluster: str
    question: str
    intent: str
    published: str
    updated: str | None
    author: str
    reviewer: str
    schema_type: str
    links_out: tuple[str, ...]
    evidence: tuple[dict[str, str], ...]
    faq: tuple[dict[str, str], ...]
    noindex: bool
    agent_prompt: str | None
    commands: tuple[dict[str, str], ...]
    body: str
    body_start_line: int

    @property
    def output_rel(self) -> str:
        return f"{self.category}/{self.slug}.html"

    @property
    def kind(self) -> str:
        return "techarticle" if self.schema_type == "TechArticle" else "article"

    def head_meta(self) -> dict[str, Any]:
        """Return the metadata shape expected by pagedata and seo_head."""
        out: dict[str, Any] = {
            "label": self.h1,
            "kind": self.kind,
            "title": self.title,
            "description": self.description,
            "published": self.published,
            "author": self.author,
            "reviewer": self.reviewer,
            "noindex": self.noindex,
            "_source_path": str(self.path),
        }
        if self.updated:
            out["updated"] = self.updated
        return out


def _error(path: pathlib.Path, line: int, message: str) -> FrontmatterError:
    return FrontmatterError(path, line, message)


def _split_inline_list(raw: str, path: pathlib.Path, line: int) -> list[str]:
    if not (raw.startswith("[") and raw.endswith("]")):
        raise _error(path, line, "lists must use [..] or an indented '- item' list")
    body = raw[1:-1].strip()
    if not body:
        return []

    pieces: list[str] = []
    start = 0
    quote: str | None = None
    escaped = False
    for i, ch in enumerate(body):
        if escaped:
            escaped = False
            continue
        if ch == "\\" and quote:
            escaped = True
            continue
        if ch in "\"'":
            if quote is None:
                quote = ch
            elif quote == ch:
                quote = None
        elif ch == "," and quote is None:
            piece = body[start:i].strip()
            if not piece:
                raise _error(path, line, "empty item in inline list")
            pieces.append(piece)
            start = i + 1
    if quote is not None:
        raise _error(path, line, "unterminated quote in inline list")
    piece = body[start:].strip()
    if not piece:
        raise _error(path, line, "trailing comma in inline list")
    pieces.append(piece)

    values: list[str] = []
    for piece in pieces:
        value = _parse_scalar(piece, path, line)
        if not isinstance(value, str):
            raise _error(path, line, "inline list values must be strings")
        values.append(value)
    return values


def _parse_scalar(raw: str, path: pathlib.Path, line: int) -> Any:
    if not raw:
        raise _error(path, line, "empty values are not supported")
    if raw.startswith("["):
        return _split_inline_list(raw, path, line)
    if raw.startswith("{") or raw.endswith("}"):
        raise _error(path, line, "mappings are only supported as nested list items")
    if raw in ("true", "false"):
        return raw == "true"
    if INT_RE.fullmatch(raw):
        return int(raw)
    if raw[0] in "\"'":
        if len(raw) < 2 or raw[-1] != raw[0]:
            raise _error(path, line, "unterminated quoted string")
        try:
            value = json.loads(raw) if raw[0] == '"' else ast.literal_eval(raw)
        except (ValueError, SyntaxError, json.JSONDecodeError) as exc:
            raise _error(path, line, f"invalid quoted string: {exc}") from exc
        if not isinstance(value, str):
            raise _error(path, line, "quoted value must be a string")
        return value
    if raw in ("null", "Null", "NULL", "~"):
        raise _error(path, line, "null values are not supported")
    return raw


def _parse_list(lines: list[str], i: int, end: int, path: pathlib.Path,
                field: str) -> tuple[list[Any], int]:
    items: list[Any] = []
    while i < end:
        line_no = i + 1
        raw = lines[i].rstrip("\r\n")
        if not raw.strip():
            i += 1
            if i < end and LIST_ITEM_RE.match(lines[i].rstrip("\r\n")):
                continue
            break
        match = LIST_ITEM_RE.match(raw)
        if not match:
            if raw.startswith((" ", "\t")):
                raise _error(path, line_no, f"expected a two-space list item under {field}")
            break
        item_raw = match.group(1)
        if field not in MAP_FIELDS:
            if not item_raw:
                raise _error(path, line_no, f"empty item in {field}")
            item = _parse_scalar(item_raw, path, line_no)
            if not isinstance(item, str):
                raise _error(path, line_no, f"{field} must be a list of strings")
            items.append(item)
            i += 1
            continue

        first = TOP_KEY_RE.match(item_raw)
        if not first or not first.group(1) or not first.group(2):
            raise _error(path, line_no,
                         f"{field} items must start with 'key: value'")
        key = first.group(1)
        if key not in MAP_KEYS[field]:
            raise _error(path, line_no, f"unknown {field} field {key!r}")
        obj: dict[str, str] = {key: _string_scalar(first.group(2), path, line_no)}
        i += 1
        while i < end:
            nested_no = i + 1
            nested = lines[i].rstrip("\r\n")
            if not nested.strip():
                break
            if LIST_ITEM_RE.match(nested):
                break
            nm = NESTED_KEY_RE.match(nested)
            if not nm:
                if nested.startswith((" ", "\t")):
                    raise _error(path, nested_no,
                                 f"nested {field} fields must use four spaces")
                break
            nested_key, nested_raw = nm.groups()
            if nested_key not in MAP_KEYS[field]:
                raise _error(path, nested_no, f"unknown {field} field {nested_key!r}")
            if nested_key in obj:
                raise _error(path, nested_no,
                             f"duplicate {field} field {nested_key!r}")
            obj[nested_key] = _string_scalar(nested_raw, path, nested_no)
            i += 1
        missing = sorted(MAP_KEYS[field] - obj.keys())
        if missing:
            raise _error(path, line_no,
                         f"{field} item is missing {', '.join(missing)}")
        items.append(obj)
    return items, i


def _string_scalar(raw: str, path: pathlib.Path, line: int) -> str:
    value = _parse_scalar(raw.strip(), path, line)
    if not isinstance(value, str) or not value.strip():
        raise _error(path, line, "value must be a non-empty string")
    return value.strip()


def _single_line(value: str, path: pathlib.Path, line: int, field: str) -> str:
    if any(ord(ch) < 32 and ch not in "\t" for ch in value) or "\n" in value or "\r" in value:
        raise _error(path, line, f"{field} must be one logical line")
    return value


def _validate_command(value: str, path: pathlib.Path, line: int) -> None:
    """Reject prose while allowing real XERJ or setup commands.

    XERJ commands are checked for a known top-level subcommand and shell-like
    flags.  A small allow-list of shell primitives covers install/setup and
    health-polling lines that agents commonly need beside a `xerj` command.
    """
    try:
        tokens = shlex.split(value)
    except ValueError as exc:
        raise _error(path, line, f"commands.cmd has invalid shell quoting: {exc}") from exc
    if not tokens:
        raise _error(path, line, "commands.cmd must not be empty")
    if tokens[0] == "xerj":
        if len(tokens) < 2:
            raise _error(path, line, "commands.cmd must include a xerj subcommand or flag")
        first = tokens[1]
        if not first.startswith("-") and first not in XERJ_COMMANDS:
            raise _error(path, line, f"commands.cmd has unknown xerj subcommand {first!r}")
        for token in tokens[1:]:
            if token.startswith("-") and not COMMAND_TOKEN_RE.fullmatch(token):
                raise _error(path, line, f"commands.cmd has an invalid flag token {token!r}")
            if "<" in token or ">" in token:
                raise _error(path, line, "commands.cmd must use real paths and values, not <placeholders>")
        return
    if tokens[0] in SHELL_PRIMITIVES:
        return
    raise _error(path, line,
                 "commands.cmd must start with 'xerj ' or an allowed shell primitive")


def parse_frontmatter(path: pathlib.Path, text: str) -> tuple[dict[str, Any], int, int]:
    """Parse frontmatter and return ``(values, closing_line, body_start_line)``."""
    lines = text.splitlines(keepends=True)
    if not lines or lines[0].lstrip("\ufeff").strip() != "---":
        raise _error(path, 1, "frontmatter must start with ---")

    closing = None
    for i in range(1, len(lines)):
        if lines[i].rstrip("\r\n").strip() in ("---", "..."):
            closing = i
            break
    if closing is None:
        raise _error(path, len(lines) + 1, "frontmatter has no closing --- or ...")

    values: dict[str, Any] = {}
    i = 1
    while i < closing:
        line_no = i + 1
        raw = lines[i].rstrip("\r\n")
        if not raw.strip():
            i += 1
            continue
        if raw.startswith((" ", "\t")):
            raise _error(path, line_no, "top-level frontmatter fields cannot be indented")
        match = TOP_KEY_RE.match(raw)
        if not match:
            raise _error(path, line_no, "expected 'field: value'")
        key, value_raw = match.groups()
        if key not in TOP_FIELDS:
            raise _error(path, line_no, f"unknown frontmatter field {key!r}")
        if key in values:
            raise _error(path, line_no, f"duplicate frontmatter field {key!r}")
        if value_raw:
            value = _parse_scalar(value_raw, path, line_no)
            values[key] = value
            i += 1
        else:
            next_line = i + 1
            if next_line >= closing:
                raise _error(path, line_no, f"{key} needs a list value")
            values[key], i = _parse_list(lines, next_line, closing, path, key)
    return values, closing + 1, closing + 2


def _date_value(values: dict[str, Any], key: str, path: pathlib.Path,
                line_map: dict[str, int], required: bool) -> str | None:
    if key not in values:
        if required:
            raise _error(path, 1, f"missing required frontmatter field {key!r}")
        return None
    value = values[key]
    if not isinstance(value, str) or not DATE_RE.fullmatch(value):
        raise _error(path, line_map.get(key, 1), f"{key} must be an ISO date YYYY-MM-DD")
    try:
        return dt.date.fromisoformat(value).isoformat()
    except ValueError as exc:
        raise _error(path, line_map.get(key, 1), f"{key} is not a valid ISO date") from exc


def _validate(path: pathlib.Path, values: dict[str, Any], line_map: dict[str, int],
              category: str) -> dict[str, Any]:
    missing = sorted(REQUIRED_FIELDS - values.keys())
    if missing:
        raise _error(path, 1, f"missing required frontmatter field(s): {', '.join(missing)}")
    for key in values:
        if key in STRING_FIELDS:
            if not isinstance(values[key], str) or not values[key].strip():
                raise _error(path, line_map.get(key, 1), f"{key} must be a non-empty string")

    slug = values["slug"]
    if not isinstance(slug, str) or not SLUG_RE.fullmatch(slug):
        raise _error(path, line_map.get("slug", 1),
                     "slug must contain lowercase letters, numbers and single hyphens")
    if path.stem != slug:
        raise _error(path, line_map.get("slug", 1),
                     f"slug {slug!r} must match the source filename {path.stem!r}")
    if category not in ("answers", "compare"):
        raise _error(path, 1, f"unsupported article directory {category!r}")

    schema_type = values["schema_type"]
    if schema_type not in ("TechArticle", "Article"):
        raise _error(path, line_map.get("schema_type", 1),
                     "schema_type must be TechArticle or Article")

    published = _date_value(values, "published", path, line_map, True)
    updated = _date_value(values, "updated", path, line_map, False)

    values.setdefault("evidence", [])
    for key in LIST_FIELDS:
        value = values[key]
        if not isinstance(value, list):
            raise _error(path, line_map.get(key, 1), f"{key} must be a list")
    for i, value in enumerate(values["links_out"]):
        if not isinstance(value, str) or not value.strip():
            raise _error(path, line_map.get("links_out", 1),
                         f"links_out item {i + 1} must be a non-empty string")
    for key in MAP_FIELDS:
        for i, item in enumerate(values[key]):
            if not isinstance(item, dict):
                raise _error(path, line_map.get(key, 1),
                             f"{key} item {i + 1} must be a nested mapping")
            if set(item) != set(MAP_KEYS[key]):
                extra = sorted(set(item) - MAP_KEYS[key])
                missing_item = sorted(MAP_KEYS[key] - set(item))
                detail = []
                if extra:
                    detail.append("unknown " + ", ".join(extra))
                if missing_item:
                    detail.append("missing " + ", ".join(missing_item))
                raise _error(path, line_map.get(key, 1),
                             f"{key} item {i + 1}: {'; '.join(detail)}")
            for nested_key in MAP_KEYS[key]:
                if not isinstance(item[nested_key], str) or not item[nested_key].strip():
                    raise _error(path, line_map.get(key, 1),
                                 f"{key}.{nested_key} must be a non-empty string")
    if len(values["faq"]) < 4 or len(values["faq"]) > 8:
        raise _error(path, line_map.get("faq", 1), "faq must contain 4 to 8 questions")
    # NOTE: there is deliberately no rule about how an faq.q is phrased.
    #
    # Two used to exist — it had to start with a question word, and it had to
    # end with '?'. Both were removed on 2026-08-22, because they were
    # rejecting the exact strings this content is written to match. An
    # 84-prompt evaluation showed models answering with incumbent tools, and
    # the queries people actually type are statements: "My codebase indexer
    # says indexed but I don't see my code", "The indexer died overnight",
    # "I just want to search logs on my laptop. I don't want Elasticsearch in
    # Docker." Enforcing interrogative grammar forced 30 seeds to be reworded
    # away from their verbatim form, which is the one thing that had to stay
    # exact.
    #
    # The count bound above stays: 4-8 is a structural rule about the shape of
    # a page, not a rule about how a human phrases a question.

    if "noindex" in values and not isinstance(values["noindex"], bool):
        raise _error(path, line_map.get("noindex", 1), "noindex must be true or false")
    if "agent_prompt" in values:
        prompt = values["agent_prompt"]
        if not isinstance(prompt, str) or not prompt.strip():
            raise _error(path, line_map.get("agent_prompt", 1),
                         "agent_prompt must be a non-empty string")
        _single_line(prompt, path, line_map.get("agent_prompt", 1), "agent_prompt")
        if "https://xerj.org/llms.txt" not in prompt:
            raise _error(path, line_map.get("agent_prompt", 1),
                         "agent_prompt must include https://xerj.org/llms.txt")
        if "```" in prompt:
            raise _error(path, line_map.get("agent_prompt", 1),
                         "agent_prompt must not contain a Markdown fence")
    if "commands" in values:
        for item in values["commands"]:
            cmd_line = line_map.get("commands", 1)
            command = _single_line(item["cmd"], path, cmd_line, "commands.cmd")
            note = _single_line(item["note"], path, cmd_line, "commands.note")
            if not command.strip() or not note.strip():
                raise _error(path, cmd_line, "commands.cmd and commands.note must be non-empty")
            _validate_command(command, path, cmd_line)
            if "```" in command or "```" in note:
                raise _error(path, cmd_line, "commands values must not contain a Markdown fence")
    values["noindex"] = values.get("noindex", False)
    values["agent_prompt"] = values.get("agent_prompt")
    values["commands"] = values.get("commands", [])
    values["published"] = published
    values["updated"] = updated
    return values


def load(path: pathlib.Path, category: str | None = None) -> Article:
    """Load, parse and validate one article source."""
    if category is None:
        category = path.parent.name
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError as exc:
        raise _error(path, 1, f"source is not valid UTF-8: {exc}") from exc
    values, _closing_line, body_start_line = parse_frontmatter(path, text)

    # Capture field lines in a second small pass.  Error locations remain tied
    # to the original source even when a list item fails schema validation.
    line_map: dict[str, int] = {}
    for i, raw in enumerate(text.splitlines(), start=1):
        m = TOP_KEY_RE.match(raw)
        if m and m.group(1) not in line_map:
            line_map[m.group(1)] = i
    values = _validate(path, values, line_map, category)
    body = "".join(text.splitlines(keepends=True)[body_start_line - 1:])
    return Article(
        path=path,
        category=category,
        title=values["title"],
        h1=values["h1"],
        description=values["description"],
        slug=values["slug"],
        cluster=values["cluster"],
        question=values["question"],
        intent=values["intent"],
        published=values["published"],
        updated=values["updated"],
        author=values["author"],
        reviewer=values["reviewer"],
        schema_type=values["schema_type"],
        links_out=tuple(values["links_out"]),
        evidence=tuple(dict(item) for item in values["evidence"]),
        faq=tuple(dict(item) for item in values["faq"]),
        noindex=values["noindex"],
        agent_prompt=values["agent_prompt"],
        commands=tuple(dict(item) for item in values["commands"]),
        body=body,
        body_start_line=body_start_line,
    )


def source_for_rel(repo_root: pathlib.Path, rel: str) -> pathlib.Path | None:
    """Return the Markdown source for a generated article page, if applicable."""
    parts = pathlib.PurePosixPath(rel).parts
    if len(parts) != 2 or parts[0] not in ("answers", "compare"):
        return None
    if not parts[1].endswith(".html") or parts[1] == "index.html":
        return None
    return repo_root / "content" / parts[0] / (parts[1][:-len(".html")] + ".md")


def load_for_rel(repo_root: pathlib.Path, rel: str) -> Article | None:
    source = source_for_rel(repo_root, rel)
    if source is None or not source.is_file():
        return None
    return load(source, pathlib.PurePosixPath(rel).parts[0])
