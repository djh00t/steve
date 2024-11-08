# ai_agent/agents/research/research_agent.py

"""
Research agent implementation for web browsing and content gathering.
"""
import logging
from datetime import datetime
from typing import Any, Dict, List, Optional
from uuid import UUID, uuid4

from pydantic import BaseModel, Field

from ...core.message_bus import Message
from ...sandbox.browser.secure_browser import SecureBrowser
from ..base import BaseAgent

logger = logging.getLogger(__name__)


class SearchParams(BaseModel):
    """Search parameters."""

    query: str
    max_results: int = 5
    timeout: int = 30
    wait_for: Optional[str] = None
    filters: Dict[str, Any] = Field(default_factory=dict)


class SearchResult(BaseModel):
    """Individual search result."""

    url: str
    title: str
    description: Optional[str] = None
    content: Dict[str, Any] = Field(default_factory=dict)
    timestamp: datetime = Field(default_factory=datetime.utcnow)
    metadata: Dict[str, Any] = Field(default_factory=dict)


class ResearchSession(BaseModel):
    """Active research session."""

    id: UUID = Field(default_factory=uuid4)
    query: str
    results: List[SearchResult] = Field(default_factory=list)
    created_at: datetime = Field(default_factory=datetime.utcnow)
    updated_at: datetime = Field(default_factory=datetime.utcnow)
    status: str = "active"


class ResearchAgent(BaseAgent):
    """Agent for web research and content gathering."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.browser = SecureBrowser()
        self.active_sessions: Dict[UUID, ResearchSession] = {}

    async def execute_task(self, task: Dict[str, Any]) -> Dict[str, Any]:
        """Execute a research task."""
        action = task.get("action")
        params = task.get("params", {})

        try:
            if action == "search":
                return await self._perform_search(params)
            elif action == "analyze":
                return await self._analyze_results(params)
            else:
                raise ValueError(f"Unknown action: {action}")

        except Exception as e:
            logger.error(f"Research task failed: {str(e)}")
            return {"success": False, "error": str(e)}

    async def _perform_search(self, params: Dict[str, Any]) -> Dict[str, Any]:
        """Perform a web search."""
        search_params = SearchParams(**params)

        # Create new research session
        session = ResearchSession(query=search_params.query)
        self.active_sessions[session.id] = session

        try:
            # Use secure browser to perform search
            result = await self.browser.browse(
                url=f"https://duckduckgo.com/?q={search_params.query}",
                timeout=search_params.timeout,
            )

            if result["success"]:
                search_result = SearchResult(
                    url=result["url"], title=result["title"], content=result["content"]
                )
                session.results.append(search_result)
                session.updated_at = datetime.utcnow()

                return {
                    "success": True,
                    "session_id": session.id,
                    "results": [result.dict() for result in session.results],
                }
            else:
                return {"success": False, "error": result.get("error", "Search failed")}

        except Exception as e:
            return {"success": False, "error": str(e)}

    async def _analyze_results(self, params: Dict[str, Any]) -> Dict[str, Any]:
        """Analyze search results."""
        session_id = UUID(params["session_id"])
        session = self.active_sessions.get(session_id)

        if not session:
            raise ValueError(f"Session {session_id} not found")

        analysis = {
            "query": session.query,
            "result_count": len(session.results),
            "duration": (session.updated_at - session.created_at).total_seconds(),
            "domains": {},
            "key_terms": set(),
        }

        # Analyze results
        for result in session.results:
            # Count domains
            domain = result.url.split("/")[2]
            analysis["domains"][domain] = analysis["domains"].get(domain, 0) + 1

            # Extract key terms (simplified)
            if result.content.get("text"):
                words = result.content["text"].lower().split()
                analysis["key_terms"].update(word for word in words if len(word) > 4)

        return {"success": True, "analysis": analysis}

    async def cleanup_session(self, session_id: UUID):
        """Clean up a research session."""
        if session_id in self.active_sessions:
            del self.active_sessions[session_id]
