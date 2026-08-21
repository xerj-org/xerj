#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
ste_check.py - ASD-STE100 (Simplified Technical English) checker for XERJ content.

Dependency-free. Python 3.8+. No pip installs, standard library only.

Reads Markdown article sources and reports style violations as `file:line:col`.
Every rule carries its own rationale and its own honest accuracy note at the point
it is defined below; rule IDs (STE0xx) are stable and self-describing.

Usage
-----
    python3 scripts/seo/ste_check.py article.md
    python3 scripts/seo/ste_check.py --fail-on WARN docs/*.md
    python3 scripts/seo/ste_check.py --json --show INFO article.md
    python3 scripts/seo/ste_check.py --stats article.md
    python3 scripts/seo/ste_check.py --self-test
    cat page.txt | python3 scripts/seo/ste_check.py -

Severity tiers
--------------
    ERROR  mechanically certain; a false positive is a bug in this script
    WARN   high confidence, small ambiguous tail; a human decides
    INFO   genuine heuristic; useful in review, too noisy to gate on

Exit codes
----------
    0  no finding at or above --fail-on
    1  findings at or above --fail-on
    2  usage or I/O error
"""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import sys

# --------------------------------------------------------------------------------------
# Severity
# --------------------------------------------------------------------------------------

ERROR, WARN, INFO = "ERROR", "WARN", "INFO"
SEV_ORDER = {ERROR: 3, WARN: 2, INFO: 1}


class Finding:
    __slots__ = ("path", "line", "col", "sev", "rule", "msg", "excerpt")

    def __init__(self, path, line, col, sev, rule, msg, excerpt=""):
        self.path = path
        self.line = line
        self.col = col
        self.sev = sev
        self.rule = rule
        self.msg = msg
        self.excerpt = excerpt

    def as_dict(self):
        return {
            "path": self.path,
            "line": self.line,
            "col": self.col,
            "severity": self.sev,
            "rule": self.rule,
            "message": self.msg,
            "excerpt": self.excerpt,
        }

    def __repr__(self):
        return "%s:%d:%d: %s %s %s" % (
            self.path, self.line, self.col, self.sev, self.rule, self.msg
        )


# --------------------------------------------------------------------------------------
# Limits (STYLE-ste100.md sections 3.4 and 4.1)
# --------------------------------------------------------------------------------------

LIMIT_PROCEDURAL = 20          # hard, ERROR above
LIMIT_DESCRIPTIVE = 25         # soft, WARN above
LIMIT_DESCRIPTIVE_HARD = 32    # hard, ERROR above
LIMIT_PARA_SENTENCES = 3       # AEO tightening, WARN above
LIMIT_PARA_SENTENCES_HARD = 6  # ASD-STE100 limit, ERROR above
LIMIT_NOUN_CLUSTER = 3         # INFO at 4, WARN at 5+
TLDR_MIN_WORDS = 30
TLDR_MAX_WORDS = 60
FAQ_QUESTION_MAX_WORDS = 12

# --------------------------------------------------------------------------------------
# Section 6: approved terminology.
#
# Each entry: (concept, canonical, [(regex, note), ...], max_per_doc)
# `max_per_doc` > 0 permits the gloss allowance of STYLE-ste100.md section 4.3.
# Regexes run against text with code spans, code fences and URLs already masked out.
# --------------------------------------------------------------------------------------

TERMS = [
    # --- core actions -------------------------------------------------------------
    ("make files searchable", "index", [
        (r"\b(?<!turbo-)(?<!bulk )ingest(?:s|ed|ing|ion)?\b"
         r"(?!\s+(?:pipeline|pipelines|path|paths|endpoint|endpoints|api|API|"
         r"throughput|rate|rates|performance|benchmark|benchmarks|speed|latency|"
         r"worker|workers|phase|phases|mode|modes))", ""),
        (r"\bcrawl(?:s|ed|ing|er|ers)?\b", ""),
        (r"\bscan(?:s|ned|ning|ner)?\b(?!\s+with\s+filters)", "use 'index'; 'exact scan' is only a kNN execution mode"),
        (r"\bslurp(?:s|ed|ing)?\b", ""),
        (r"\bharvest(?:s|ed|ing)?\b", ""),
        (r"\bhoover(?:s|ed|ing)?\b", ""),
    ], 0),
    ("start a node", "start", [
        (r"\bspin(?:s|ning)?\s+up\b", ""),
        (r"\bstand(?:s|ing)?\s+up\b", ""),
        (r"\bfire(?:s|d)?\s+up\b", ""),
        (r"\bboot(?:s|ed|ing)?\s+up\b", ""),
        (r"\bbring(?:s)?\s+up\b", ""),
        (r"\bcommence(?:s|d)?\b", ""),
        (r"\binitiate(?:s|d)?\b", ""),
    ], 0),
    ("stop a node", "stop", [
        (r"\btear\s+down\b", ""),
        (r"\bteardown\b", ""),
        (r"\btake\s+down\b", ""),
    ], 0),
    ("remove data", "delete", [
        (r"\bpurge(?:s|d|ing)?\b", ""),
        (r"\bwipe(?:s|d)?\b", ""),
        (r"\bnuke(?:s|d)?\b", ""),
        (r"\berase(?:s|d)?\b", ""),
    ], 0),
    ("confirm a result", "make sure", [
        (r"\bensure(?:s|d)?\b", ""),
        (r"\bvalidate(?:s|d)?\b(?!\s+(?:the\s+)?(?:schema|json|JSON))", ""),
        (r"\bdouble-check\b", ""),
    ], 0),
    ("use", "use", [
        (r"\bleverag(?:e|es|ed|ing)\b", ""),
        (r"\butiliz(?:e|es|ed|ing)\b", ""),
        (r"\butilis(?:e|es|ed|ing)\b", ""),
    ], 0),
    ("approximation", "about", [
        (r"\bapproximately\b", ""),
        (r"\broughly\b", ""),
        (r"\bcirca\b", ""),
        (r"\bin\s+the\s+region\s+of\b", ""),
    ], 0),
    ("cause", "because", [
        (r"\bdue\s+to\s+the\s+fact\s+that\b", ""),
        (r"\bowing\s+to\b", ""),
    ], 0),
    ("condition", "if", [
        (r"\bin\s+the\s+event\s+that\b", ""),
    ], 0),
    ("capability", "can", [
        (r"\bis\s+able\s+to\b", ""),
        (r"\bare\s+able\s+to\b", ""),
        (r"\bhas\s+the\s+ability\s+to\b", ""),
        (r"\b(?:is|are)\s+capable\s+of\b", ""),
    ], 0),
    ("obligation", "must", [
        (r"\bshall\b", ""),
        (r"\bis\s+required\s+to\b", ""),
        (r"\bneeds?\s+to\b", ""),
    ], 0),
    ("time order", "before / after", [
        (r"\bprior\s+to\b", ""),
        (r"\bsubsequent\s+to\b", ""),
    ], 0),
    ("purpose", "to", [
        (r"\bin\s+order\s+to\b", ""),
        (r"\bso\s+as\s+to\b", ""),
        (r"\bfor\s+the\s+purpose\s+of\b", ""),
    ], 0),

    # --- retrieval ----------------------------------------------------------------
    ("lexical retrieval", "full-text search", [
        (r"\bkeyword\s+search\b", ""),
        (r"\bfree-?text\s+search\b", ""),
        (r"\blexical\s+search\b", ""),
    ], 0),
    ("lexical ranking", "BM25", [
        (r"\bTF-?IDF\b", "XERJ ranks with BM25, not TF-IDF"),
        (r"\bOkapi\b", ""),
        (r"\bkeyword\s+scoring\b", ""),
        (r"\blexical\s+ranking\b", ""),
    ], 0),
    ("retrieval by meaning", "semantic search", [
        (r"\bneural\s+search\b", ""),
        (r"\bmeaning\s+search\b", ""),
        (r"\bconceptual\s+search\b", ""),
        (r"\bsimilarity\s+search\b", ""),
    ], 0),
    ("nearest-neighbor retrieval", "kNN", [
        (r"CS:\bKNN\b", "write it exactly as kNN"),
        (r"CS:\bk-NN\b", "write it exactly as kNN"),
        (r"CS:(?<![/_.\w])knn(?![_\w])", "write it exactly as kNN"),
        (r"CS:\bANN\b", "say 'approximate kNN'"),
        (r"\bnearest[- ]neighbour\b", "American spelling: nearest-neighbor"),
    ], 0),
    ("combined retrieval", "hybrid search", [
        (r"\bblended\s+(?:search|retrieval)\b", ""),
        (r"\bfused\s+search\b", ""),
        (r"\bmixed\s+search\b", ""),
        (r"\bcombined\s+search\b", ""),
        (r"\bhybrid\s+retrieval\b", ""),
    ], 1),  # one gloss per article, STYLE-ste100.md 4.3
    ("fusion algorithm", "Reciprocal Rank Fusion (RRF)", [
        (r"\bRRF\s+fusion\b", "redundant"),
        (r"(?<!Reciprocal )\brank\s+fusion\b", ""),
        (r"\breciprocal\s+fusion\b", ""),
        (r"\bRRF\s+algorithm\b", ""),
    ], 0),
    ("numeric representation", "embedding", [
        (r"\bvector\s+embedding(?:s)?\b", ""),
        (r"\bembedding\s+vector(?:s)?\b", ""),
        (r"\bfeature\s+vector(?:s)?\b", ""),
        (r"\bfloat\s+array(?:s)?\b", ""),
    ], 0),
    ("the embedding component", "embedder", [
        (r"\bvectorizer(?:s)?\b", ""),
        (r"\bembedding\s+model(?:s)?\b", ""),
        (r"\bembedding\s+engine(?:s)?\b", ""),
    ], 0),
    ("a retrievable piece of a document", "passage", [
        (r"\bsnippet(?:s)?\b", "'passage' for retrieved text; 'segment' is a storage file"),
        (r"\bfragment(?:s)?\b", ""),
    ], 0),

    # --- storage and topology -----------------------------------------------------
    ("a stored searchable dataset", "index", [
        (r"\bcollection(?:s)?\b", ""),
        (r"\bdata\s?store(?:s)?\b", ""),
        (r"\b(?:the|all|these|those|its|your|our|their|both|many|some|several|\d+)\s+indexes\b",
         "the plural is 'indices'"),
        (r"\bindexes\s+(?:are|were|live|exist|contain|hold)\b", "the plural is 'indices'"),
    ], 0),
    ("a stored item", "document", [
        (r"\brecord(?:s)?\b", "'records' is allowed only in verbatim CLI output"),
        (r"\bdoc\b", "a stored item is a document"),
    ], 0),
    ("one running XERJ process", "node", [
        (r"\bservers\b", "countable: use 'nodes'"),
        (r"\binstance(?:s)?\b", ""),
        (r"\bbox(?:es)?\b", ""),
    ], 0),
    ("range partition inside an index", "region", [
        (r"\btablet(?:s)?\b", ""),
    ], 0),
    ("point-in-time copy", "snapshot", [
        (r"\bdump(?:s|ed|ing)?\b", ""),
    ], 0),

    # --- agent-facing -------------------------------------------------------------
    ("an LLM-driven program", "agent", [
        (r"\bbot(?:s)?\b", ""),
        (r"\bcopilot(?:s)?\b", ""),
        (r"\bAI\s+assistant(?:s)?\b", ""),
    ], 0),
    ("the model input budget", "context window", [
        (r"\bcontext\s+length\b", ""),
        (r"\bcontext\s+size\b", ""),
        (r"\bprompt\s+window\b", ""),
        (r"\btoken\s+budget\b", ""),
    ], 0),
    ("the relationship layer", "second brain", [
        (r"\bknowledge\s+graph(?:s)?\b", "allowed once, as the category gloss"),
        (r"\bmemory\s+graph\b", ""),
        (r"\bmind\s+map\b", ""),
        (r"\blink\s+graph\b", ""),
    ], 1),
    ("durable agent recall", "agent memory", [
        (r"\blong-?term\s+memory\b", ""),
        (r"\bpersistent\s+memory\b", ""),
        (r"\bmemory\s+store\b", ""),
    ], 0),
    ("the MCP integration", "MCP server", [
        (r"\bMCP\s+(?:endpoint|bridge|adapter|gateway|proxy|connector)(?:s)?\b", ""),
    ], 0),
    ("the retrieval-before-writing loop", "reference coding", [
        (r"\bRAG\s+for\s+code\b", ""),
        (r"\bcontext\s+engineering\b", ""),
    ], 0),
    ("the wire protocol", "Elasticsearch REST API", [
        (r"CS:\bElasticSearch\b", "one capital S: Elasticsearch"),
        (r"CS:(?<![/\w-])elasticsearch(?![\w./-])", "capitalize it: Elasticsearch"),
        (r"CS:\bES\s+API\b", ""),
        (r"CS:\bES-compatible\b", ""),
    ], 0),
]

# Precompile. A `CS:` prefix on a pattern means "match case-sensitively" - needed for
# acronym-shape rules such as kNN vs KNN vs knn, and Elasticsearch vs elasticsearch.
COMPILED_TERMS = []
for _concept, _canonical, _pats, _maxdoc in TERMS:
    _out = []
    for _pat, _note in _pats:
        if _pat.startswith("CS:"):
            _out.append((re.compile(_pat[3:]), _note))
        else:
            _out.append((re.compile(_pat, re.I), _note))
    COMPILED_TERMS.append((_concept, _canonical, _out, _maxdoc))

# Patterns that do not apply inside a heading. A heading names a section or a subsystem,
# so "## Ingest at line rate" names the ingest path rather than using 'ingest' as a
# synonym for 'index'. Measured on landing/docs/quickstart.html, this was the only
# false positive the ERROR tier produced.
HEADING_EXEMPT = {
    r"\b(?<!turbo-)(?<!bulk )ingest(?:s|ed|ing|ion)?\b"
    r"(?!\s+(?:pipeline|pipelines|path|paths|endpoint|endpoints|api|API|"
    r"throughput|rate|rates|performance|benchmark|benchmarks|speed|latency|"
    r"worker|workers|phase|phases|mode|modes))",
}

# `backup` is context-dependent: banned in body prose, allowed in a heading.
BACKUP_RE = re.compile(r"\bback-?ups?\b", re.I)

# --------------------------------------------------------------------------------------
# Marketing / weasel words (section 3.1 A-3) - ERROR, mechanically certain
# --------------------------------------------------------------------------------------

MARKETING = [
    r"blazing(?:ly)?[ -]fast", r"lightning[ -]fast", r"screaming(?:ly)?[ -]fast",
    r"seamless(?:ly)?", r"effortless(?:ly)?", r"frictionless(?:ly)?",
    r"revolutionary", r"game[ -]chang(?:er|ing)", r"cutting[ -]edge",
    r"state[ -]of[ -]the[ -]art", r"best[ -]in[ -]class", r"world[ -]class",
    r"next[ -]gen(?:eration)?", r"industry[ -]leading", r"unparalleled",
    r"unmatched", r"turnkey", r"supercharge[sd]?", r"turbocharge[sd]?",
    r"unleash(?:es|ed)?", r"empower(?:s|ed|ing)?", r"delight(?:s|ful)?",
    r"magical(?:ly)?", r"magic", r"simply", r"just\s+(?=works|run|add|use|point|set)",
    r"easy", r"easily", r"powerful", r"robust", r"blazing", r"amazing",
    r"incredible", r"awesome", r"stunning", r"insane(?:ly)?",
]
MARKETING_RE = re.compile(r"\b(" + "|".join(MARKETING) + r")\b", re.I)

# --------------------------------------------------------------------------------------
# Idioms (3.1 A-4), Latinisms (A-5), contractions (A-6), British spelling (A-7)
# --------------------------------------------------------------------------------------

IDIOMS = [
    r"out\s+of\s+the\s+box", r"under\s+the\s+hood", r"on\s+the\s+fly",
    r"rule\s+of\s+thumb", r"at\s+the\s+end\s+of\s+the\s+day", r"silver\s+bullet",
    r"heavy\s+lifting", r"low[- ]hanging\s+fruit", r"no[- ]brainer",
    r"in\s+a\s+nutshell", r"moving\s+parts", r"bread\s+and\s+butter",
    r"hit\s+the\s+ground\s+running", r"ballpark", r"secret\s+sauce",
    r"first[- ]class\s+citizen", r"batteries\s+included", r"drop\s+in\s+the\s+ocean",
    r"apples\s+to\s+apples", r"boil\s+the\s+ocean", r"move\s+the\s+needle",
    r"table\s+stakes", r"north\s+star", r"deep\s+dive", r"dogfood(?:ing|s)?",
]
IDIOM_RE = re.compile(r"\b(" + "|".join(IDIOMS) + r")\b", re.I)

LATINISMS = [
    (r"\be\.\s?g\.", "for example"),
    (r"\bi\.\s?e\.", "that is"),
    (r"\betc\b\.?", "and so on, or finish the list"),
    (r"\bvia\b", "through, or with"),
    (r"\bper\s+se\b", "in itself"),
    (r"\bvs\b\.?", "compared with"),
    (r"\bcf\b\.?", "compare"),
    (r"\bN\.B\.", "note"),
    (r"\bad\s+hoc\b", "one-off"),
    (r"\bde\s+facto\b", "in practice"),
    (r"\bviz\b\.?", "namely"),
    (r"\bvice\s+versa\b", "the other way round"),
]
LATIN_RES = [(re.compile(p), sug) for p, sug in LATINISMS]

CONTRACTIONS = [
    r"\w+n['’]t", r"\b(?:I|you|we|they)['’](?:re|ve|ll|d)\b",
    r"\b(?:he|she|it)['’](?:s|ll|d)\b",
    r"\b(?:that|there|here|what|who|let|where|how)['’]s\b",
    r"\bI['’]m\b", r"\bcan['’]t\b", r"\bwon['’]t\b",
]
CONTRACTION_RE = re.compile(r"(" + "|".join(CONTRACTIONS) + r")", re.I)

BRITISH = {
    "behaviour": "behavior", "behaviours": "behaviors", "colour": "color",
    "colours": "colors", "favour": "favor", "honour": "honor", "labour": "labor",
    "neighbour": "neighbor", "neighbours": "neighbors", "neighbouring": "neighboring",
    "organise": "organize", "organised": "organized", "organising": "organizing",
    "organisation": "organization", "optimise": "optimize", "optimised": "optimized",
    "optimisation": "optimization", "memorise": "memorize", "memorised": "memorized",
    "recognise": "recognize", "recognised": "recognized", "analyse": "analyze",
    "analysed": "analyzed", "normalise": "normalize", "normalised": "normalized",
    "initialise": "initialize", "initialised": "initialized", "serialise": "serialize",
    "serialised": "serialized", "prioritise": "prioritize", "summarise": "summarize",
    "specialise": "specialize", "authorise": "authorize", "customise": "customize",
    "synchronise": "synchronize", "visualise": "visualize", "minimise": "minimize",
    "maximise": "maximize", "centre": "center", "centres": "centers",
    "metre": "meter", "metres": "meters", "fibre": "fiber", "defence": "defense",
    "offence": "offense", "catalogue": "catalog", "grey": "gray",
    "whilst": "while", "amongst": "among", "travelled": "traveled",
    "cancelled": "canceled", "modelling": "modeling", "labelled": "labeled",
    "licence": "license",
}
BRITISH_RE = re.compile(r"\b(" + "|".join(sorted(BRITISH, key=len, reverse=True)) + r")\b", re.I)

FORWARD_REFS = [
    r"as\s+(?:mentioned|noted|described|discussed|explained)\s+(?:above|below|earlier|previously)",
    r"see\s+(?:below|above)", r"as\s+we\s+(?:saw|will\s+see)",
    r"in\s+the\s+(?:previous|next|following|preceding)\s+(?:section|chapter|paragraph)",
    r"later\s+in\s+this\s+(?:article|post|page|guide)",
    r"read\s+on", r"keep\s+reading", r"stay\s+tuned",
    r"we(?:'ll|\s+will)\s+(?:look\s+at|cover|explore|walk\s+through|dive\s+into)",
]
FORWARD_REF_RE = re.compile(r"\b(" + "|".join(FORWARD_REFS) + r")\b", re.I)

HEDGE_MODAL_RE = re.compile(r"\b(may|might|should|could|would|perhaps|possibly|probably)\b", re.I)

# --------------------------------------------------------------------------------------
# Passive voice (STE020 / STE021)
# --------------------------------------------------------------------------------------

BE_FORMS = r"(?:is|are|was|were|be|been|being|am)"
PASSIVE_ADVERBS = (
    r"(?:not|also|already|often|usually|only|then|now|still|never|always|generally|"
    r"typically|automatically|explicitly|implicitly|currently|actually|simply|"
    r"normally|therefore|however|first|later|immediately|directly|fully|partly)"
)
IRREGULAR_PARTICIPLES = {
    "written", "built", "made", "given", "taken", "done", "shown", "known", "seen",
    "found", "held", "kept", "sent", "put", "read", "run", "brought", "bought",
    "caught", "taught", "thought", "told", "sold", "left", "lost", "meant", "met",
    "paid", "said", "spent", "understood", "drawn", "driven", "chosen", "broken",
    "spoken", "frozen", "hidden", "forgotten", "overwritten", "rewritten", "cut",
    "split", "shut", "hit", "let", "cost", "begun", "thrown", "grown", "drawn",
    "torn", "worn", "won", "set", "sung", "swept", "dealt", "felt", "become",
}
# Participial adjectives suppressed to keep the false-positive rate low (section 8.3).
ADJECTIVAL_PARTICIPLES = {
    "required", "limited", "related", "based", "involved", "interested", "located",
    "situated", "dedicated", "advanced", "detailed", "complicated", "experienced",
    "qualified", "closed", "enabled", "disabled", "tired", "pleased", "worried",
    "concerned", "prepared", "determined", "unlimited", "unrelated", "unlimited",
    "intended", "suited", "aimed", "geared", "tied", "bound", "aware", "unchanged",
}
NOT_PARTICIPLES = {
    "speed", "indeed", "embed", "exceed", "proceed", "succeed", "need", "feed",
    "seed", "breed", "greed", "sacred", "hundred", "naked", "wicked", "agreed",
    "shed", "sled", "misled",
}
PASSIVE_RE = re.compile(
    r"\b(" + BE_FORMS + r")\b((?:\s+" + PASSIVE_ADVERBS + r"){0,2})\s+([A-Za-z][A-Za-z-]{3,})\b",
    re.I,
)
AGENT_RE = re.compile(r"^\W*(?:\w+\W+){0,2}by\b", re.I)

# --------------------------------------------------------------------------------------
# Gerunds (STE030 / STE031)
# --------------------------------------------------------------------------------------

ALLOWED_ING = {
    "indexing", "autoindexing", "embedding", "embeddings", "chunking", "clustering",
    "routing", "sharding", "ranking", "scoring", "logging", "mapping", "mappings",
    "encoding", "encodings", "tracing", "warning", "warnings", "setting", "settings",
    "tokenizing", "profiling", "monitoring", "networking", "engineering", "training",
    "onboarding", "benchmarking", "batching", "caching", "hashing", "matching",
    "highlighting", "filtering", "sorting", "paging", "streaming", "sampling",
    "scripting", "reindexing", "snapshotting", "provisioning", "partitioning",
}
NON_GERUND_ING = {
    "during", "string", "strings", "thing", "things", "something", "nothing",
    "anything", "everything", "being", "king", "ring", "spring", "wing", "ceiling",
    "morning", "evening", "meaning", "meanings", "sibling", "siblings",
    "listing", "listings", "heading", "headings", "reading", "readings",
}
ING_SUPPRESS = ALLOWED_ING | NON_GERUND_ING
ING_WORD_RE = re.compile(r"^([A-Za-z][A-Za-z-]{3,}ing)\b")
PROGRESSIVE_RE = re.compile(
    r"\b(" + BE_FORMS + r")\b((?:\s+" + PASSIVE_ADVERBS + r"){0,2})\s+([A-Za-z][A-Za-z-]{3,}ing)\b",
    re.I,
)
PREP_ING_RE = re.compile(
    r"\b(by|for|after|before|without|while|when|through|of|in|on|with)\s+([A-Za-z][A-Za-z-]{3,}ing)\b",
    re.I,
)

# --------------------------------------------------------------------------------------
# Noun clusters (STE040)
# --------------------------------------------------------------------------------------

NON_NOUN = set("""
a an the this that these those and or but nor so yet for of in on at to from by with
without into onto over under above below between among across through during before
after since until while about against per via as if then than because although though
unless whether when where why how what which who whom whose it its they them their there
here we us our you your i me my he she his her one two three four five six seven eight
nine ten is are was were be been being am has have had do does did can could will would
shall should may might must use uses used using make makes made get gets got need needs
run runs take takes give gives see sees know knows want wants add adds set sets put show
shows find finds keep keeps let lets start starts stop stops write writes send sends
store stores read reads work works come comes go goes say says call calls turn turns
new old fast slow large small big high low good bad same other each every all any some
more most less few many single static local remote default live real full open free
first next last own only such both no not now also still just very too much long short
same able available possible simple complex different several such own current entire
whole main key common typical usual likely unlikely
already instead again once twice rather together directly indirectly alone apart
official native explicit implicit optional required internal external upstream downstream
asks ask serves serve connects connect returns return provides provide supports support
handles handle accepts accept sends send stores store holds hold builds build creates
create indexes queries query searches search reads read writes write runs turns applies
apply emits emit yields yield fails fail passes pass counts count maps map lists list
picks pick drops drop splits split merges merge scores score ranks rank costs cost
tells tell shows show means mean matters matter helps help
""".split())
ADJ_SUFFIX_RE = re.compile(r"(?:ly|able|ible|ous|ful|less|ive|ish|est|er)$", re.I)
NOUNISH_RE = re.compile(r"^[A-Za-z][A-Za-z0-9-]*$")

# --------------------------------------------------------------------------------------
# Pronoun openers (STE080)
# --------------------------------------------------------------------------------------

BARE_PRONOUNS = {"it", "they", "them", "its", "their", "he", "she", "him", "her"}
DEMONSTRATIVES = {"this", "that", "these", "those"}
DEMONSTRATIVE_VERB_RE = re.compile(
    r"^(?:is|are|was|were|means|gives|makes|lets|allows|can|will|has|have|does|do|"
    r"provides|creates|works|happens|matters|helps|keeps|becomes|remains|leaves|"
    r"avoids|costs|saves|takes|requires|removes|adds|turns|sounds|looks|seems)\b",
    re.I,
)

# --------------------------------------------------------------------------------------
# Procedural detection
# --------------------------------------------------------------------------------------

PROC_HEADING_RE = re.compile(
    r"\b(install|quickstart|quick start|getting started|get started|setup|set up|"
    r"steps?|procedure|how to|walkthrough|configure|configuration|upgrade|migrate|"
    r"migration|run it|try it|deploy|deployment|first (?:index|query|run|boot))\b",
    re.I,
)
IMPERATIVE_VERBS = {
    "run", "start", "stop", "install", "open", "add", "set", "export", "copy",
    "paste", "point", "create", "delete", "pass", "send", "use", "download",
    "edit", "replace", "restart", "wait", "enter", "click", "choose", "select",
    "index", "query", "search", "build", "clone", "make", "put", "read", "write",
    "check", "connect", "define", "remove", "call", "give", "keep", "apply",
    "confirm", "extract", "load", "save", "type", "press", "drop", "push", "pull",
}
IMPERATIVE_OPEN_RE = re.compile(r"^([A-Za-z]+)\b")

ABBREVIATIONS = {
    "e.g", "i.e", "vs", "mr", "mrs", "dr", "prof", "no", "fig", "sec", "eq",
    "approx", "cf", "al", "inc", "ltd", "st", "jan", "feb", "mar", "apr", "jun",
    "jul", "aug", "sep", "oct", "nov", "dec", "min", "max", "sq", "cmd",
}


# --------------------------------------------------------------------------------------
# Masking: strip code, URLs and comments while preserving line/column geometry
# --------------------------------------------------------------------------------------

def _pad(text, target_len, fill=" "):
    if len(text) >= target_len:
        return text[:target_len]
    return text + fill * (target_len - len(text))


def mask_line(line):
    """Blank out inline code, URLs and images. Keeps the line the same length."""
    # images: ![alt](url) -> alt
    line = re.sub(r"!\[([^\]]*)\]\(([^)]*)\)",
                  lambda m: _pad(m.group(1), len(m.group(0))), line)
    # links: [text](url) -> text
    line = re.sub(r"\[([^\]]*)\]\(([^)]*)\)",
                  lambda m: _pad(m.group(1), len(m.group(0))), line)
    # reference-style link definitions
    line = re.sub(r"^\s*\[[^\]]+\]:\s*\S+.*$", lambda m: " " * len(m.group(0)), line)
    # autolinks and bare URLs
    line = re.sub(r"<https?://[^>]*>", lambda m: " " * len(m.group(0)), line)
    line = re.sub(r"https?://\S+", lambda m: "x" * len(m.group(0)), line)
    # inline code -> one opaque token of the same width (counts as one word)
    line = re.sub(r"`[^`]*`", lambda m: "x" * len(m.group(0)), line)
    # HTML comments
    line = re.sub(r"<!--.*?-->", lambda m: " " * len(m.group(0)), line)
    # inline HTML tags
    line = re.sub(r"</?[A-Za-z][^>]*>", lambda m: " " * len(m.group(0)), line)
    return line


IGNORE_LINE_RE = re.compile(r"<!--\s*ste:ignore\s*-->")
IGNORE_START_RE = re.compile(r"<!--\s*ste:ignore-start\s*-->")
IGNORE_END_RE = re.compile(r"<!--\s*ste:ignore-end\s*-->")


# --------------------------------------------------------------------------------------
# Sentence splitting and word counting
# --------------------------------------------------------------------------------------

def split_sentences(text):
    """Return [(sentence_text, offset_in_text), ...]."""
    out = []
    start = 0
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch in ".!?":
            j = i
            while j + 1 < n and text[j + 1] in ".!?":
                j += 1
            after = text[j + 1:j + 2]
            before = text[max(0, i - 1):i]
            # decimal number or version: 158.1, v1.2
            if ch == "." and before.isdigit() and after.isdigit():
                i = j + 1
                continue
            # known abbreviation
            m = re.search(r"([A-Za-z.]+)$", text[start:i])
            if ch == "." and m and m.group(1).lower().strip(".") in ABBREVIATIONS:
                i = j + 1
                continue
            # single-letter initial
            if ch == "." and re.search(r"(?:^|\s)[A-Z]$", text[start:i]):
                i = j + 1
                continue
            if after == "" or after.isspace():
                nxt = text[j + 1:].lstrip()
                if nxt == "" or nxt[0].isupper() or nxt[0].isdigit() or nxt[0] in "\"'`([_*#-“":
                    sent = text[start:j + 1].strip()
                    if sent:
                        off = start + (len(text[start:j + 1]) - len(text[start:j + 1].lstrip()))
                        out.append((sent, off))
                    start = j + 1
            i = j + 1
            continue
        i += 1
    tail = text[start:].strip()
    if tail:
        off = start + (len(text[start:]) - len(text[start:].lstrip()))
        out.append((tail, off))
    return out


WORD_RE = re.compile(r"[A-Za-z0-9À-ɏ]")


def count_words(sentence):
    return sum(1 for t in sentence.split() if WORD_RE.search(t))


# --------------------------------------------------------------------------------------
# Document model
# --------------------------------------------------------------------------------------

class Block:
    __slots__ = ("kind", "lines", "start_line", "text", "heading_path", "level",
                 "procedural", "in_faq", "raw_first")

    def __init__(self, kind, start_line):
        self.kind = kind            # heading | paragraph | list | table | quote | code
        self.lines = []             # [(lineno, masked_text)]
        self.start_line = start_line
        self.text = ""
        self.heading_path = []
        self.level = 0
        self.procedural = False
        self.in_faq = False
        self.raw_first = ""


FENCE_RE = re.compile(r"^\s{0,3}(```+|~~~+)")
HEADING_RE = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
OL_RE = re.compile(r"^\s{0,6}(\d+)[.)]\s+(.*)$")
UL_RE = re.compile(r"^\s{0,6}([-*+])\s+(.*)$")
TABLE_RE = re.compile(r"^\s{0,3}\|")
QUOTE_RE = re.compile(r"^\s{0,3}>")
TLDR_RE = re.compile(r"^\s{0,3}(?:[*_]{0,2})TL[;:]?\s?DR(?:[*_]{0,2})\s*[—–:.-]?\s*(.*)$", re.I)
FAQ_HEADING_RE = re.compile(r"^(?:FAQ|FAQs|Frequently\s+asked\s+questions?)\s*$", re.I)
FRONT_H1_RE = re.compile(r"^h1\s*:\s*(.*?)\s*$", re.I)
QUESTION_WORD_RE = re.compile(
    r"^(what|why|how|when|where|which|who|whom|whose|does|do|did|is|are|was|were|"
    r"can|could|should|will|would|may|might|has|have|must)\b", re.I)


def parse(path, raw_lines):
    """Turn markdown lines into blocks with heading context. Returns (blocks, meta)."""
    blocks = []
    meta = {"h1": [], "tldr": None, "faq_h2_line": None, "faq_items": [],
            "ignored_lines": set()}

    heading_path = []
    in_fence = False
    fence_marker = None
    in_front_matter = False
    ignoring = False
    cur = None
    in_faq = False

    def flush():
        nonlocal cur
        if cur is not None and cur.lines:
            cur.text = " ".join(t.strip() for _, t in cur.lines).strip()
            if cur.text:
                blocks.append(cur)
        cur = None

    for idx, raw in enumerate(raw_lines, start=1):
        stripped = raw.rstrip("\n")

        if idx == 1 and stripped.strip() == "---":
            in_front_matter = True
            continue
        if in_front_matter:
            # Article sources keep the rendered H1 in frontmatter so the
            # generator, rather than the Markdown body, owns the only <h1>.
            # Count that field for STE100 while leaving other metadata out of
            # the prose checks.
            front_h1 = FRONT_H1_RE.match(stripped)
            if front_h1 and front_h1.group(1):
                value = front_h1.group(1).strip()
                if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                    value = value[1:-1]
                meta["h1"].append((idx, value))
            if stripped.strip() in ("---", "..."):
                in_front_matter = False
            continue

        if IGNORE_START_RE.search(stripped):
            ignoring = True
        if IGNORE_END_RE.search(stripped):
            ignoring = False
            meta["ignored_lines"].add(idx)
            continue
        if ignoring or IGNORE_LINE_RE.search(stripped):
            meta["ignored_lines"].add(idx)
            continue

        fm = FENCE_RE.match(stripped)
        if fm:
            if not in_fence:
                in_fence = True
                fence_marker = fm.group(1)[0]
                flush()
            elif stripped.strip()[0] == fence_marker:
                in_fence = False
                fence_marker = None
            continue
        if in_fence:
            continue

        if not stripped.strip():
            flush()
            continue

        hm = HEADING_RE.match(stripped)
        if hm:
            flush()
            level = len(hm.group(1))
            title = mask_line(hm.group(2)).strip()
            heading_path = heading_path[:level - 1]
            while len(heading_path) < level - 1:
                heading_path.append("")
            heading_path.append(title)
            b = Block("heading", idx)
            b.level = level
            b.lines = [(idx, title)]
            b.heading_path = list(heading_path)
            b.raw_first = hm.group(2)
            if level == 1:
                meta["h1"].append((idx, title))
            if level == 2:
                in_faq = bool(FAQ_HEADING_RE.match(title))
                if in_faq:
                    meta["faq_h2_line"] = idx
            elif level > 2 and in_faq:
                meta["faq_items"].append((idx, title))
            b.in_faq = in_faq
            b.text = title
            blocks.append(b)
            continue

        masked = mask_line(stripped)

        tm = TLDR_RE.match(masked)
        if tm and meta["tldr"] is None:
            flush()
            cur = Block("paragraph", idx)
            cur.heading_path = list(heading_path)
            cur.in_faq = in_faq
            cur.lines.append((idx, tm.group(1)))
            meta["tldr"] = cur
            continue

        if TABLE_RE.match(stripped):
            if cur is None or cur.kind != "table":
                flush()
                cur = Block("table", idx)
                cur.heading_path = list(heading_path)
                cur.in_faq = in_faq
            cur.lines.append((idx, masked))
            continue

        if QUOTE_RE.match(stripped):
            if cur is None or cur.kind != "quote":
                flush()
                cur = Block("quote", idx)
                cur.heading_path = list(heading_path)
                cur.in_faq = in_faq
            cur.lines.append((idx, re.sub(r"^\s{0,3}>\s?", "", masked)))
            continue

        om = OL_RE.match(stripped)
        um = UL_RE.match(stripped)
        if om or um:
            flush()
            cur = Block("list", idx)
            cur.heading_path = list(heading_path)
            cur.in_faq = in_faq
            cur.procedural = bool(om)
            body = mask_line(om.group(2) if om else um.group(2))
            pad = len(stripped) - len(body)
            cur.lines.append((idx, " " * max(pad, 0) + body))
            continue

        if cur is None:
            cur = Block("paragraph", idx)
            cur.heading_path = list(heading_path)
            cur.in_faq = in_faq
        cur.lines.append((idx, masked))

    flush()

    # procedural classification
    for b in blocks:
        if b.kind in ("heading", "table"):
            continue
        head_ctx = " > ".join(h for h in b.heading_path if h)
        if b.procedural:
            continue
        if PROC_HEADING_RE.search(head_ctx):
            b.procedural = True
            continue
        first = b.text.lstrip()
        m = IMPERATIVE_OPEN_RE.match(first)
        if m and m.group(1).lower() in IMPERATIVE_VERBS:
            b.procedural = True

    return blocks, meta


# --------------------------------------------------------------------------------------
# Locating a match back to (line, col)
# --------------------------------------------------------------------------------------

def locate(block, offset):
    """Map an offset inside block.text back to (lineno, col). block.text joins lines
    with a single space after stripping, so we walk the same construction."""
    pos = 0
    for lineno, text in block.lines:
        t = text.strip()
        if not t:
            continue
        if pos + len(t) >= offset:
            lead = len(text) - len(text.lstrip())
            return lineno, lead + (offset - pos) + 1
        pos += len(t) + 1
    if block.lines:
        return block.lines[-1][0], 1
    return block.start_line, 1


# --------------------------------------------------------------------------------------
# The checks
# --------------------------------------------------------------------------------------

class Checker:
    def __init__(self, path, raw_lines, opts):
        self.path = path
        self.raw_lines = raw_lines
        self.opts = opts
        self.findings = []
        self.term_seen = {}

    def add(self, block, offset, sev, rule, msg, excerpt=""):
        line, col = locate(block, offset)
        if line in self.meta["ignored_lines"]:
            return
        if rule in self.opts.skip:
            return
        if self.opts.only and rule not in self.opts.only:
            return
        self.findings.append(Finding(self.path, line, col, sev, rule, msg, excerpt))

    def run(self):
        self.blocks, self.meta = parse(self.path, self.raw_lines)
        for b in self.blocks:
            self.check_block(b)
        self.check_structure()
        self.findings.sort(key=lambda f: (f.line, f.col, f.rule))
        return self.findings

    # -- per block ---------------------------------------------------------------

    def check_block(self, b):
        text = b.text
        if not text:
            return

        # terminology and lexical checks run on every block kind
        self.check_terminology(b, text)
        self.check_marketing(b, text)
        self.check_idioms(b, text)
        self.check_latinisms(b, text)
        if b.kind != "quote":
            self.check_contractions(b, text)
        self.check_british(b, text)
        self.check_forward_refs(b, text)

        if b.kind in ("heading", "table"):
            return

        sentences = split_sentences(text)

        # STE010 paragraph sentence count
        if b.kind == "paragraph" and len(sentences) > LIMIT_PARA_SENTENCES:
            sev = ERROR if len(sentences) > LIMIT_PARA_SENTENCES_HARD else WARN
            self.add(b, 0, sev, "STE010",
                     "paragraph has %d sentences (max %d; ASD-STE100 hard max %d)"
                     % (len(sentences), LIMIT_PARA_SENTENCES, LIMIT_PARA_SENTENCES_HARD))

        for si, (sent, off) in enumerate(sentences):
            self.check_sentence_length(b, sent, off)
            self.check_passive(b, sent, off)
            self.check_gerunds(b, sent, off)
            self.check_noun_clusters(b, sent, off)
            self.check_modals(b, sent, off)
            if b.procedural:
                self.check_one_instruction(b, sent, off)
            if si > 0:
                self.check_pronoun_opener(b, sent, off)

    # -- individual rules --------------------------------------------------------

    def check_sentence_length(self, b, sent, off):
        n = count_words(sent)
        if b.procedural:
            if n > self.opts.limit_procedural:
                self.add(b, off, ERROR, "STE001",
                         "procedural sentence is %d words (max %d)"
                         % (n, self.opts.limit_procedural), sent[:90])
        else:
            if n > self.opts.limit_descriptive_hard:
                self.add(b, off, ERROR, "STE002",
                         "descriptive sentence is %d words (hard max %d)"
                         % (n, self.opts.limit_descriptive_hard), sent[:90])
            elif n > self.opts.limit_descriptive:
                self.add(b, off, WARN, "STE002",
                         "descriptive sentence is %d words (target max %d)"
                         % (n, self.opts.limit_descriptive), sent[:90])

    def check_passive(self, b, sent, off):
        for m in PASSIVE_RE.finditer(sent):
            part = m.group(3).lower()
            if part in ADJECTIVAL_PARTICIPLES or part in NOT_PARTICIPLES:
                continue
            is_part = part in IRREGULAR_PARTICIPLES or (
                part.endswith("ed") and len(part) >= 5 and part not in NOT_PARTICIPLES
            )
            if not is_part:
                continue
            if part.endswith("ing"):
                continue
            tail = sent[m.end():]
            agent = bool(AGENT_RE.match(tail))
            frag = m.group(0)
            if agent:
                sev = ERROR if b.procedural else WARN
                self.add(b, off + m.start(), sev, "STE020",
                         "passive voice with a named agent: '%s ... by ...'" % frag, frag)
            else:
                sev = WARN if b.procedural else INFO
                self.add(b, off + m.start(), sev, "STE021",
                         "possible passive voice: '%s' (heuristic)" % frag, frag)

    def check_gerunds(self, b, sent, off):
        m = ING_WORD_RE.match(sent.lstrip("*_“\"'"))
        if m and m.group(1).lower() not in ING_SUPPRESS:
            self.add(b, off, WARN, "STE030",
                     "sentence opens with an '-ing' form: '%s'" % m.group(1), sent[:70])
        for pm in PROGRESSIVE_RE.finditer(sent):
            word = pm.group(3).lower()
            if word in NON_GERUND_ING:
                continue
            self.add(b, off + pm.start(), WARN, "STE030",
                     "progressive tense '%s'; use a simple tense" % pm.group(0).strip(),
                     pm.group(0))
        for pm in PREP_ING_RE.finditer(sent):
            word = pm.group(2).lower()
            if word in ING_SUPPRESS:
                continue
            self.add(b, off + pm.start(), INFO, "STE031",
                     "'-ing' after a preposition: '%s'" % pm.group(0), pm.group(0))

    def check_noun_clusters(self, b, sent, off):
        for clause_m in re.finditer(r"[^,;:.!?()\[\]—–\"]+", sent):
            clause = clause_m.group(0)
            base = clause_m.start()
            run = []
            run_start = None
            for tm in re.finditer(r"\S+", clause):
                tok = tm.group(0).strip("\"'*_`()[]")
                low = tok.lower()
                nounish = (
                    bool(NOUNISH_RE.match(tok))
                    and low not in NON_NOUN
                    and not ADJ_SUFFIX_RE.search(low)
                    and not (low.endswith("ed") and low not in ALLOWED_ING)
                    and not (low.endswith("ing") and low not in ALLOWED_ING)
                    and len(tok) > 1
                    and set(low) != {"x"}
                )
                if nounish:
                    if not run:
                        run_start = tm.start()
                    run.append(tok)
                else:
                    self._emit_cluster(b, off + base + (run_start or 0), run)
                    run = []
                    run_start = None
            self._emit_cluster(b, off + base + (run_start or 0), run)

    def _emit_cluster(self, b, offset, run):
        if len(run) <= LIMIT_NOUN_CLUSTER:
            return
        sev = WARN if len(run) >= LIMIT_NOUN_CLUSTER + 2 else INFO
        self.add(b, offset, sev, "STE040",
                 "noun cluster of %d: '%s' (max %d; heuristic, no POS tagger)"
                 % (len(run), " ".join(run), LIMIT_NOUN_CLUSTER), " ".join(run))

    def check_modals(self, b, sent, off):
        for m in HEDGE_MODAL_RE.finditer(sent):
            self.add(b, off + m.start(), INFO, "STE110",
                     "hedging modal '%s'; STE allows 'can' and 'must'" % m.group(0),
                     m.group(0))

    def check_one_instruction(self, b, sent, off):
        m = re.search(r"\band\s+then\b", sent, re.I)
        if m:
            self.add(b, off + m.start(), WARN, "STE120",
                     "two instructions in one sentence ('and then'); split the step",
                     sent[:80])
            return
        first = IMPERATIVE_OPEN_RE.match(sent.lstrip())
        if first and first.group(1).lower() in IMPERATIVE_VERBS:
            m2 = re.search(r",?\s+and\s+([a-z]+)\b", sent)
            if m2 and m2.group(1).lower() in IMPERATIVE_VERBS:
                self.add(b, off + m2.start(), WARN, "STE120",
                         "two imperatives in one sentence ('%s ... and %s')"
                         % (first.group(1).lower(), m2.group(1).lower()), sent[:80])

    def check_pronoun_opener(self, b, sent, off):
        toks = sent.lstrip("*_“\"'").split()
        if not toks:
            return
        first = re.sub(r"[^A-Za-z]", "", toks[0]).lower()
        if not first:
            return
        rest = " ".join(toks[1:])
        if first in BARE_PRONOUNS:
            self.add(b, off, INFO, "STE080",
                     "sentence opens with '%s'; name the referent so the chunk stands alone"
                     % toks[0], sent[:70])
        elif first in DEMONSTRATIVES and DEMONSTRATIVE_VERB_RE.match(rest):
            self.add(b, off, INFO, "STE080",
                     "sentence opens with bare '%s'; name the referent" % toks[0],
                     sent[:70])

    def check_terminology(self, b, text):
        for concept, canonical, patterns, max_per_doc in COMPILED_TERMS:
            for rx, note in patterns:
                if b.kind == "heading" and rx.pattern in HEADING_EXEMPT:
                    continue
                for m in rx.finditer(text):
                    key = (concept, rx.pattern)
                    self.term_seen[key] = self.term_seen.get(key, 0) + 1
                    if self.term_seen[key] <= max_per_doc:
                        continue
                    msg = "banned synonym '%s' for '%s' (concept: %s)" % (
                        m.group(0), canonical, concept)
                    if note:
                        msg += " - " + note
                    if max_per_doc:
                        msg += " - the gloss allowance is %d per article" % max_per_doc
                    self.add(b, m.start(), ERROR, "STE050", msg, m.group(0))
        if b.kind != "heading":
            for m in BACKUP_RE.finditer(text):
                if re.match(r"back-?ups?\s+and\s+restore", m.group(0), re.I):
                    continue
                self.add(b, m.start(), ERROR, "STE050",
                         "banned synonym '%s' for 'snapshot' (concept: point-in-time copy)"
                         " - 'backup' is allowed only in a nav label or H1" % m.group(0),
                         m.group(0))

    def check_marketing(self, b, text):
        for m in MARKETING_RE.finditer(text):
            self.add(b, m.start(), ERROR, "STE060",
                     "marketing or weasel word '%s'; state the fact instead" % m.group(0),
                     m.group(0))

    def check_idioms(self, b, text):
        for m in IDIOM_RE.finditer(text):
            self.add(b, m.start(), WARN, "STE072",
                     "idiom '%s'; idioms do not survive translation or chunking"
                     % m.group(0), m.group(0))

    def check_latinisms(self, b, text):
        for rx, sug in LATIN_RES:
            for m in rx.finditer(text):
                self.add(b, m.start(), WARN, "STE071",
                         "Latinism '%s'; use '%s'" % (m.group(0), sug), m.group(0))

    def check_contractions(self, b, text):
        for m in CONTRACTION_RE.finditer(text):
            self.add(b, m.start(), WARN, "STE070",
                     "contraction '%s'; write it out" % m.group(0), m.group(0))

    def check_british(self, b, text):
        for m in BRITISH_RE.finditer(text):
            w = m.group(0)
            if w.lower() == "licence" and re.search(
                    r"contributor\s+licence", text[max(0, m.start() - 12):m.end()], re.I):
                continue
            self.add(b, m.start(), WARN, "STE073",
                     "British spelling '%s'; STE mandates American spelling ('%s')"
                     % (w, BRITISH[w.lower()]), w)

    def check_forward_refs(self, b, text):
        for m in FORWARD_REF_RE.finditer(text):
            self.add(b, m.start(), WARN, "STE130",
                     "forward or backward reference '%s'; link to the anchor instead"
                     % m.group(0), m.group(0))

    # -- document structure ------------------------------------------------------

    def check_structure(self):
        if not self.opts.article:
            return
        h1s = self.meta["h1"]
        pseudo = Block("paragraph", 1)
        pseudo.lines = [(1, "")]
        if len(h1s) != 1:
            self.findings.append(Finding(
                self.path, h1s[1][0] if len(h1s) > 1 else 1, 1, ERROR, "STE100",
                "expected exactly 1 H1, found %d" % len(h1s)))

        tldr = self.meta["tldr"]
        if tldr is None:
            self.findings.append(Finding(
                self.path, 1, 1, WARN, "STE101",
                "no TL;DR block found; every article needs one after the H1"))
        else:
            tldr.text = " ".join(t.strip() for _, t in tldr.lines).strip()
            n = count_words(tldr.text)
            if n < TLDR_MIN_WORDS or n > TLDR_MAX_WORDS:
                self.findings.append(Finding(
                    self.path, tldr.start_line, 1, WARN, "STE102",
                    "TL;DR is %d words; the AEO extraction window is %d-%d"
                    % (n, TLDR_MIN_WORDS, TLDR_MAX_WORDS)))

        for lineno, title in self.meta["faq_items"]:
            if not title.rstrip().endswith("?"):
                self.findings.append(Finding(
                    self.path, lineno, 1, ERROR, "STE090",
                    "FAQ heading is not a question: '%s'" % title, title))
                continue
            if not QUESTION_WORD_RE.match(title.strip()):
                self.findings.append(Finding(
                    self.path, lineno, 1, WARN, "STE091",
                    "FAQ question does not start with a question word: '%s'" % title,
                    title))
            if count_words(title) > FAQ_QUESTION_MAX_WORDS:
                self.findings.append(Finding(
                    self.path, lineno, 1, WARN, "STE091",
                    "FAQ question is %d words (max %d); phrase it as a user would type it"
                    % (count_words(title), FAQ_QUESTION_MAX_WORDS), title))


# --------------------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------------------

class Opts:
    def __init__(self, **kw):
        self.limit_procedural = kw.get("limit_procedural", LIMIT_PROCEDURAL)
        self.limit_descriptive = kw.get("limit_descriptive", LIMIT_DESCRIPTIVE)
        self.limit_descriptive_hard = kw.get("limit_descriptive_hard", LIMIT_DESCRIPTIVE_HARD)
        self.article = kw.get("article", True)
        self.skip = set(kw.get("skip", ()))
        self.only = set(kw.get("only", ()))


def check_text(text, path="<text>", opts=None):
    opts = opts or Opts()
    return Checker(path, text.splitlines(True), opts).run()


def check_file(path, opts):
    with io.open(path, "r", encoding="utf-8", errors="replace") as fh:
        return Checker(path, fh.readlines(), opts).run()


def main(argv=None):
    ap = argparse.ArgumentParser(
        prog="ste_check.py",
        description="ASD-STE100 checker for XERJ markdown content. "
                    "Each STE0xx rule is documented at its definition in this file.")
    ap.add_argument("paths", nargs="*", help="markdown files, or - for stdin")
    ap.add_argument("--fail-on", default=ERROR, choices=[ERROR, WARN, INFO],
                    help="exit 1 when a finding at or above this tier exists (default ERROR)")
    ap.add_argument("--show", default=INFO, choices=[ERROR, WARN, INFO],
                    help="minimum tier to print (default INFO)")
    ap.add_argument("--json", action="store_true", help="emit JSON")
    ap.add_argument("--stats", action="store_true", help="print a per-rule count")
    ap.add_argument("--no-article", action="store_true",
                    help="skip the article structure checks (H1, TL;DR, FAQ)")
    ap.add_argument("--limit-procedural", type=int, default=LIMIT_PROCEDURAL)
    ap.add_argument("--limit-descriptive", type=int, default=LIMIT_DESCRIPTIVE)
    ap.add_argument("--limit-descriptive-hard", type=int, default=LIMIT_DESCRIPTIVE_HARD)
    ap.add_argument("--skip", default="", help="comma-separated rule IDs to suppress")
    ap.add_argument("--only", default="", help="comma-separated rule IDs to run")
    ap.add_argument("--self-test", action="store_true", help="run the built-in unit tests")
    args = ap.parse_args(argv)

    if args.self_test:
        return self_test()

    if not args.paths:
        ap.error("no input files (use - for stdin, or --self-test)")

    opts = Opts(
        limit_procedural=args.limit_procedural,
        limit_descriptive=args.limit_descriptive,
        limit_descriptive_hard=args.limit_descriptive_hard,
        article=not args.no_article,
        skip=[s.strip() for s in args.skip.split(",") if s.strip()],
        only=[s.strip() for s in args.only.split(",") if s.strip()],
    )

    all_findings = []
    for p in args.paths:
        try:
            if p == "-":
                all_findings.extend(check_text(sys.stdin.read(), "<stdin>", opts))
            else:
                all_findings.extend(check_file(p, opts))
        except IOError as exc:
            sys.stderr.write("ste_check: %s\n" % exc)
            return 2

    show = SEV_ORDER[args.show]
    shown = [f for f in all_findings if SEV_ORDER[f.sev] >= show]

    if args.json:
        counts = {}
        for f in all_findings:
            counts[f.rule] = counts.get(f.rule, 0) + 1
        print(json.dumps({
            "findings": [f.as_dict() for f in shown],
            "totals": {
                "all": len(all_findings),
                "ERROR": sum(1 for f in all_findings if f.sev == ERROR),
                "WARN": sum(1 for f in all_findings if f.sev == WARN),
                "INFO": sum(1 for f in all_findings if f.sev == INFO),
            },
            "by_rule": counts,
        }, indent=2))
    else:
        for f in shown:
            print("%s:%d:%d: %s %s %s" % (f.path, f.line, f.col, f.sev, f.rule, f.msg))
        if args.stats:
            counts = {}
            for f in all_findings:
                counts.setdefault(f.rule, {"ERROR": 0, "WARN": 0, "INFO": 0})
                counts[f.rule][f.sev] += 1
            print("\n-- per-rule counts --")
            for rule in sorted(counts):
                c = counts[rule]
                print("  %-8s ERROR=%-3d WARN=%-3d INFO=%-3d" %
                      (rule, c["ERROR"], c["WARN"], c["INFO"]))
        print("\n%d finding(s): %d ERROR, %d WARN, %d INFO" % (
            len(all_findings),
            sum(1 for f in all_findings if f.sev == ERROR),
            sum(1 for f in all_findings if f.sev == WARN),
            sum(1 for f in all_findings if f.sev == INFO)))

    fail = SEV_ORDER[args.fail_on]
    return 1 if any(SEV_ORDER[f.sev] >= fail for f in all_findings) else 0


# --------------------------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------------------------

SAMPLE_COMPLIANT = """# How XERJ indexes a folder

**TL;DR** — `xerj autoindex ~/my-project` indexes a folder in one command. XERJ sniffs
each file, infers one index per dataset, and writes the mappings for you. A 25,329-file
tree took 158.1 seconds and produced 593 indices on one node.

## What autoindex does

`autoindex` runs in two phases. Phase A infers the datasets and skips junk files. Phase B
indexes the files with 8 workers.

## FAQ

### Does XERJ need a schema?

No. `autoindex` writes the field mappings from the data it finds. You can still create an
index by hand when you want an explicit encoder.
"""

SAMPLE_VIOLATING = """# XERJ: The Blazing Fast Search Engine

**TL;DR** — It's a revolutionary engine.

## Getting started

Simply spin up the server and then run the crawler, and check that the records are
ingested correctly, because the powerful indexing pipeline is designed by our team to
seamlessly handle everything you throw at it out of the box.

Running the binary, the cluster metadata replication log shard assignment table is
written by Raft. This is why it's easy. The backup can be restored via the API, e.g.
after a node failure, etc.

## FAQ

### Vector database requirements

It depends.
"""

SAMPLE_BORDERLINE = """# Hybrid search in XERJ

**TL;DR** — Hybrid search runs a BM25 sub-query and a kNN sub-query in one request, then
fuses the two result lists with Reciprocal Rank Fusion. Send it to the
Elasticsearch-compatible `_search` endpoint on port 9200. XERJ needs no separate vector
database.

## How the fusion works

XERJ scores each sub-query independently, and the fusion step then combines the two
ranked lists into a single ordered list that neither sub-query would produce alone.
The result set is written to the response in rank order. Each document keeps its
per-sub-query rank so that you can debug the fusion.

## FAQ

### Which fields can hybrid search combine in a single query?

A `text` field and a `dense_vector` field. XERJ evaluates both sub-queries in one pass.
"""


def _rules(findings):
    return sorted({f.rule for f in findings})


def _by_rule(findings, rule):
    return [f for f in findings if f.rule == rule]


def self_test():
    failures = []
    checked = [0]

    def ok(cond, label):
        checked[0] += 1
        if not cond:
            failures.append(label)

    # --- sentence splitting -----------------------------------------------------
    s = split_sentences("XERJ indexed 25,329 files in 158.1 seconds. It used 8 workers.")
    ok(len(s) == 2, "split: decimal must not end a sentence (got %d)" % len(s))
    s = split_sentences("Formats such as CSV, i.e. comma files, work. Then query it.")
    ok(len(s) == 2, "split: abbreviation must not end a sentence (got %d)" % len(s))
    s = split_sentences("One. Two. Three.")
    ok(len(s) == 3, "split: three sentences (got %d)" % len(s))

    # --- word counting ----------------------------------------------------------
    _m = mask_line("Run `xerj autoindex ~/my-project` now.")
    ok(count_words(_m) == 3, "count_words: a code span is one word (got %d)" % count_words(_m))

    # --- masking ----------------------------------------------------------------
    m = mask_line("See `simply` and [docs](https://x.org/simply).")
    ok("simply" not in m.split("docs")[0], "mask: inline code must be hidden")
    ok(len(m) == len("See `simply` and [docs](https://x.org/simply)."),
       "mask: length must be preserved")

    # --- sentence length --------------------------------------------------------
    f = check_text("## Install\n\n" + "Run the command with " + " ".join(["word"] * 20) + ".\n",
                   opts=Opts(article=False))
    ok(any(x.rule == "STE001" and x.sev == ERROR for x in f),
       "STE001: long procedural sentence must ERROR")
    f = check_text("XERJ " + " ".join(["word"] * 27) + ".\n", opts=Opts(article=False))
    ok(any(x.rule == "STE002" and x.sev == WARN for x in f),
       "STE002: 28-word descriptive sentence must WARN")
    f = check_text("XERJ " + " ".join(["word"] * 40) + ".\n", opts=Opts(article=False))
    ok(any(x.rule == "STE002" and x.sev == ERROR for x in f),
       "STE002: 41-word descriptive sentence must ERROR")
    f = check_text("XERJ indexes a folder in one command.\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE002"), "STE002: short sentence must not fire")

    # --- paragraph --------------------------------------------------------------
    f = check_text("A one. B two. C three. D four.\n", opts=Opts(article=False))
    ok(any(x.rule == "STE010" for x in f), "STE010: 4 sentences must WARN")
    f = check_text("A one. B two. C three.\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE010"), "STE010: 3 sentences must pass")

    # --- passive ----------------------------------------------------------------
    f = check_text("The log is written by Raft.\n", opts=Opts(article=False))
    ok(any(x.rule == "STE020" for x in f), "STE020: agentive passive must fire")
    f = check_text("The index is created at boot.\n", opts=Opts(article=False))
    ok(any(x.rule == "STE021" and x.sev == INFO for x in f),
       "STE021: agentless passive must be INFO in descriptive text")
    for clean in ("The API key is required.", "The port is open.",
                  "Auth is enabled by default.", "XERJ is a search engine.",
                  "The scores are based on BM25.", "Speed is limited."):
        f = check_text(clean + "\n", opts=Opts(article=False))
        ok(not _by_rule(f, "STE020"),
           "STE020 false positive on: %s" % clean)

    # --- gerunds ----------------------------------------------------------------
    f = check_text("Creating the index takes time.\n", opts=Opts(article=False))
    ok(any(x.rule == "STE030" for x in f), "STE030: -ing opener must fire")
    f = check_text("XERJ is indexing the tree.\n", opts=Opts(article=False))
    ok(any(x.rule == "STE030" for x in f), "STE030: progressive must fire")
    f = check_text("Indexing runs in two phases.\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE030"), "STE030: approved -ing noun must not fire")

    # --- noun clusters ----------------------------------------------------------
    f = check_text("Check the cluster metadata replication log shard table.\n",
                   opts=Opts(article=False))
    ok(any(x.rule == "STE040" for x in f), "STE040: 5-noun cluster must fire")
    f = check_text("XERJ writes the mappings for you.\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE040"), "STE040: short phrase must not fire")

    # --- terminology ------------------------------------------------------------
    f = check_text("The crawler scans your repository.\n", opts=Opts(article=False))
    ok(any(x.rule == "STE050" and x.sev == ERROR for x in f),
       "STE050: 'crawler' must ERROR")
    f = check_text("XERJ indexes your repository.\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE050"), "STE050: canonical wording must pass")
    f = check_text("Use the ingest pipeline for OTLP.\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE050"),
       "STE050: 'ingest pipeline' is an exempt subsystem name")
    f = check_text("Send it to `turbo-ingest` now.\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE050"), "STE050: code spans are exempt")
    f = check_text("Hybrid search, sometimes called blended retrieval, fuses two lists. "
                   "Blended retrieval is fast.\n", opts=Opts(article=False))
    ok(len(_by_rule(f, "STE050")) == 1,
       "STE050: gloss allowance permits exactly one blended-retrieval mention")

    # --- marketing --------------------------------------------------------------
    f = check_text("XERJ is a blazing fast and seamless engine.\n", opts=Opts(article=False))
    ok(len(_by_rule(f, "STE060")) >= 2, "STE060: marketing words must ERROR")
    f = check_text("XERJ indexed 25,329 files in 158.1 seconds.\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE060"), "STE060: factual sentence must pass")

    # --- contractions, latinisms, idioms, spelling ------------------------------
    f = check_text("It's fine and you don't need a JVM.\n", opts=Opts(article=False))
    ok(len(_by_rule(f, "STE070")) == 2, "STE070: two contractions expected")
    f = check_text("XERJ's index is small.\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE070"), "STE070: possessive must not fire")
    f = check_text("Formats, e.g. CSV, are handled via the sniffer.\n",
                   opts=Opts(article=False))
    ok(len(_by_rule(f, "STE071")) == 2, "STE071: 'e.g.' and 'via' expected")
    f = check_text("Vector search works out of the box.\n", opts=Opts(article=False))
    ok(any(x.rule == "STE072" for x in f), "STE072: idiom must fire")
    f = check_text("The behaviour of neighbouring regions.\n", opts=Opts(article=False))
    ok(len(_by_rule(f, "STE073")) == 2, "STE073: British spellings expected")

    # --- pronoun openers --------------------------------------------------------
    f = check_text("XERJ creates one index per dataset. It is queryable at once.\n",
                   opts=Opts(article=False))
    ok(any(x.rule == "STE080" for x in f), "STE080: bare 'It' opener must fire")
    f = check_text("XERJ creates one index per dataset. Each index is queryable.\n",
                   opts=Opts(article=False))
    ok(not _by_rule(f, "STE080"), "STE080: named referent must pass")
    f = check_text("XERJ creates indices. This index holds symbols.\n",
                   opts=Opts(article=False))
    ok(not _by_rule(f, "STE080"), "STE080: demonstrative determiner must pass")

    # --- FAQ + structure --------------------------------------------------------
    f = check_text("# T\n\n## FAQ\n\n### Vector database requirements\n\nNo.\n")
    ok(any(x.rule == "STE090" for x in f), "STE090: non-question FAQ heading must ERROR")
    f = check_text("# T\n\n## FAQ\n\n### Does XERJ need a vector database?\n\nNo.\n")
    ok(not _by_rule(f, "STE090"), "STE090: question heading must pass")
    f = check_text("# One\n\n# Two\n")
    ok(any(x.rule == "STE100" for x in f), "STE100: two H1s must ERROR")
    f = check_text("# One\n\nSome body text here.\n")
    ok(any(x.rule == "STE101" for x in f), "STE101: missing TL;DR must WARN")
    f = check_text("# One\n\n**TL;DR** — Too short.\n")
    ok(any(x.rule == "STE102" for x in f), "STE102: short TL;DR must WARN")

    # --- ignore markers ---------------------------------------------------------
    f = check_text("XERJ is blazing fast. <!-- ste:ignore -->\n", opts=Opts(article=False))
    ok(not _by_rule(f, "STE060"), "ste:ignore must suppress the line")

    # --- code fences ------------------------------------------------------------
    f = check_text("```\nsimply leverage the crawler out of the box\n```\n",
                   opts=Opts(article=False))
    ok(not f, "fenced code must be skipped entirely (got %r)" % _rules(f))

    # --- the three sample paragraphs -------------------------------------------
    comp = check_text(SAMPLE_COMPLIANT, "sample-compliant.md")
    ok(not [x for x in comp if x.sev == ERROR],
       "compliant sample must produce no ERROR (got %r)"
       % [(x.rule, x.msg) for x in comp if x.sev == ERROR])

    viol = check_text(SAMPLE_VIOLATING, "sample-violating.md")
    got = set(_rules(viol))
    want = {"STE050", "STE060", "STE070", "STE071", "STE072", "STE090", "STE102"}
    ok(want <= got, "violating sample missing rules: %r" % sorted(want - got))
    ok(len([x for x in viol if x.sev == ERROR]) >= 8,
       "violating sample should have many ERRORs (got %d)"
       % len([x for x in viol if x.sev == ERROR]))

    bord = check_text(SAMPLE_BORDERLINE, "sample-borderline.md")
    ok(not [x for x in bord if x.sev == ERROR],
       "borderline sample must not ERROR (got %r)"
       % [(x.rule, x.msg) for x in bord if x.sev == ERROR])
    ok([x for x in bord if x.sev == WARN],
       "borderline sample must produce at least one WARN")

    # --- report -----------------------------------------------------------------
    if failures:
        print("SELF-TEST FAILED: %d of %d assertion(s)" % (len(failures), checked[0]))
        for f_ in failures:
            print("  - " + f_)
        return 1
    print("SELF-TEST PASSED: %d assertions" % checked[0])
    return 0


if __name__ == "__main__":
    sys.exit(main())
