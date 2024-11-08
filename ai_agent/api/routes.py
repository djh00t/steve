# ai_agent/api/routes.py

"""API routes for the AI Agent system."""
from typing import Dict, Any
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

app = FastAPI(title="AI Agent API")


class CommandRequest(BaseModel):
    """Request model for executing commands."""

    command: str
    timeout: int = 300


class ResearchRequest(BaseModel):
    """Request model for research tasks."""

    query: str
    max_results: int = 5


@app.post("/execute")
async def execute_command(request: CommandRequest) -> Dict[str, Any]:
    """Execute a bash command."""
    try:
        # Implementation will come from bash_agent
        return {"status": "success", "message": "Command executed"}
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.post("/research")
async def research(request: ResearchRequest) -> Dict[str, Any]:
    """Perform web research."""
    try:
        # Implementation will come from research_agent
        return {"status": "success", "message": "Research completed"}
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/health")
async def health_check() -> Dict[str, str]:
    """Health check endpoint."""
    return {"status": "healthy"}
