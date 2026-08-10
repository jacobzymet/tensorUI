import base64
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
    encoded = sys.argv[-1]
    encoded += "=" * (-len(encoded) % 4)
    request = json.loads(base64.urlsafe_b64decode(encoded).decode("utf-8"))
    query = str(request.get("query", "")).strip()
    max_results = max(1, min(int(request.get("max_results", 6)), 20))
    if not query:
        fail("TENSORUI_DDGS_ERROR", "query is empty")

    raw_results = DDGS().text(query, max_results=max_results)
    results = []
    for item in raw_results or []:
        url = clean(item.get("href") or item.get("url"))
        title = clean(item.get("title"))
        if not title or not url:
            continue
        results.append(
            {
                "title": title,
                "url": url,
                "snippet": clean(item.get("body") or item.get("snippet")),
            }
        )
    # Emit locale-independent ASCII JSON. This avoids Windows console code pages
    # producing non-UTF-8 bytes while still preserving Unicode through \u escapes.
    response = json.dumps({"results": results}, ensure_ascii=True, separators=(",", ":"))
    sys.stdout.buffer.write(response.encode("ascii"))
except SystemExit:
    raise
except Exception as error:
    fail("TENSORUI_DDGS_ERROR", str(error))
