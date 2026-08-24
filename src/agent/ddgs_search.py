import json
import sys


def fail(marker: str, detail: str = "") -> None:
    sys.stderr.write(marker + (f": {detail}" if detail else ""))
    raise SystemExit(1)


def clean(value: object) -> str:
    # Some providers return lone UTF-16 surrogate code points. They are legal in
    # a Python str but not valid Unicode/UTF-8 and serde_json correctly rejects
    # them. Replace only those malformed code points while preserving real text.
    return str(value or "").encode("utf-8", "replace").decode("utf-8").strip()


if sys.version_info < (3, 10):
    fail("TENSORUI_DDGS_PYTHON_VERSION", "Python 3.10 or newer is required")

try:
    from ddgs import DDGS
except ModuleNotFoundError as error:
    if error.name == "ddgs":
        fail("TENSORUI_DDGS_NOT_INSTALLED")
    raise

try:
    if "--self-test" in sys.argv:
        response = json.dumps(
            {"ok": True, "protocol": 1}, ensure_ascii=True, separators=(",", ":")
        )
        sys.stdout.buffer.write(response.encode("ascii"))
        raise SystemExit(0)

    request = json.loads(sys.stdin.buffer.read().decode("utf-8"))
    if request.get("protocol", 1) != 1:
        fail("TENSORUI_DDGS_ERROR", "unsupported search helper protocol")
    query = str(request.get("query", "")).strip()
    max_results = max(1, min(int(request.get("max_results", 6)), 20))
    backend = str(request.get("backend", "auto")).strip().lower() or "auto"
    region = str(request.get("region", "us-en")).strip().lower() or "us-en"
    safesearch = str(request.get("safesearch", "moderate")).strip().lower()
    recency = str(request.get("recency", "any")).strip().lower()
    timelimit = {
        "any": None,
        "day": "d",
        "week": "w",
        "month": "m",
        "year": "y",
    }.get(recency)
    kind = str(request.get("kind", "web")).strip().lower() or "web"
    if kind not in ("web", "news"):
        kind = "web"
    if not query:
        fail("TENSORUI_DDGS_ERROR", "query is empty")

    ddgs = DDGS(timeout=6)
    # ddgs "auto" fans out to every engine and then waits on hung workers during
    # executor shutdown — a single search can sit until TensorUI's 45s kill.
    # Fail over a short list instead so typical queries return in a few seconds.
    auto_backends = ("duckduckgo",) if kind == "news" else ("duckduckgo", "brave", "bing")
    backends = auto_backends if backend == "auto" else (backend,)
    raw_results = []
    last_error = None
    for engine in backends:
        try:
            if kind == "news":
                raw_results = ddgs.news(
                    query,
                    region=region,
                    safesearch=safesearch,
                    timelimit=timelimit,
                    max_results=max_results,
                )
            else:
                raw_results = ddgs.text(
                    query,
                    region=region,
                    safesearch=safesearch,
                    timelimit=timelimit,
                    max_results=max_results,
                    backend=engine,
                )
            raw_results = raw_results or []
            if raw_results:
                break
        except Exception as error:
            last_error = error
            raw_results = []
    if not raw_results and last_error is not None:
        fail("TENSORUI_DDGS_ERROR", str(last_error))
    results = []
    for item in raw_results or []:
        url = clean(item.get("href") or item.get("url"))
        title = clean(item.get("title"))
        if not title or not url:
            continue
        snippet = clean(item.get("body") or item.get("snippet"))
        extra = " · ".join(
            part for part in (clean(item.get("source")), clean(item.get("date"))) if part
        )
        if extra:
            snippet = f"{snippet} ({extra})" if snippet else extra
        results.append(
            {
                "title": title,
                "url": url,
                "snippet": snippet,
            }
        )
    # Emit locale-independent ASCII JSON. This avoids Windows console code pages
    # producing non-UTF-8 bytes while still preserving Unicode through \u escapes.
    response = json.dumps(
        {"protocol": 1, "results": results}, ensure_ascii=True, separators=(",", ":")
    )
    sys.stdout.buffer.write(response.encode("ascii"))
except SystemExit:
    raise
except Exception as error:
    fail("TENSORUI_DDGS_ERROR", str(error))
