"""Google ADK agent that answers from your documents using XERJ as its
retrieval tool. The agent's LLM is Gemini; the tool is a plain function that
queries a running XERJ engine and returns cited snippets.

Run the full agent with a Gemini API key (GOOGLE_API_KEY). The tool itself —
the XERJ integration — needs no key and is verified at the bottom of this file.
"""
import json
import os
import urllib.request

from google.adk.agents import Agent

XERJ_URL = os.environ.get("XERJ_URL", "http://localhost:9200")


def search_documents(query: str, index: str = "kb") -> dict:
    """Search the user's indexed documents and return the most relevant
    passages, each with the source file it came from.

    Args:
        query: A natural-language question about the documents.
        index: The XERJ index to search (default "kb").

    Returns:
        A dict with a "results" list of {source, snippet, score}.
    """
    body = {
        "size": 3,
        "query": {"semantic": {"field": "body", "query": query, "k": 3}},
        "_source": ["body", "ax_path"],
    }
    req = urllib.request.Request(
        f"{XERJ_URL}/{index}/_search",
        json.dumps(body).encode(),
        {"Content-Type": "application/json"},
    )
    hits = json.load(urllib.request.urlopen(req))["hits"]["hits"]
    return {
        "results": [
            {
                "source": h["_source"].get("ax_path", h["_id"]),
                "snippet": h["_source"].get("body", "")[:200],
                "score": round(h.get("_score", 0.0), 3),
            }
            for h in hits
        ]
    }


# The ADK agent: Gemini reasons; XERJ retrieves. `search_documents` is passed
# as a tool, so the model calls it whenever it needs facts from the corpus.
root_agent = Agent(
    name="doc_assistant",
    model="gemini-2.5-flash",
    instruction=(
        "You answer questions about the user's documents. ALWAYS call "
        "search_documents first, then answer ONLY from the returned snippets, "
        "and cite the source file for every claim. If nothing relevant comes "
        "back, say so."
    ),
    tools=[search_documents],
)

if __name__ == "__main__":
    # Verify the XERJ integration without needing a Gemini key: call the tool
    # directly, exactly as the agent would.
    out = search_documents("database outage from too many open connections")
    print(json.dumps(out, indent=2))
    assert out["results"], "tool returned no results"
    assert "connection pool" in out["results"][0]["snippet"], "wrong top result"
    print("\nOK: the ADK tool retrieves cited answers from XERJ.")
