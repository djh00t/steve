"""
Tests for the research agent implementation.
"""
import pytest
import asyncio
from uuid import UUID, uuid4
from datetime import datetime
from unittest.mock import Mock, patch

from ai_agent.agents.research.research_agent import (
    ResearchAgent,
    SearchParams,
    SearchResult,
    ResearchSession
)
from ai_agent.sandbox.browser.secure_browser import SecureBrowser
from ai_agent.core.message_bus import MessageBus
import redis.asyncio as redis
from ai_agent.core.security_manager import SecurityContext

@pytest.fixture
def mock_browser():
    """Create a mock browser for testing."""
    with patch('ai_agent.sandbox.browser.secure_browser.SecureBrowser') as mock:
        mock.return_value = Mock(spec=SecureBrowser)
        yield mock.return_value

@pytest.fixture
async def research_agent(mock_browser):
    """Create a research agent for testing."""
    agent_id = uuid4()
    redis_client = redis.from_url("redis://localhost")
    message_bus = MessageBus(redis)
    security_context = SecurityContext(
        agent_id=agent_id,
        permissions={'web.browse'},
        auth_level=0
    )
    
    agent = ResearchAgent(
        agent_id=agent_id,
        message_bus=message_bus,
        security_context=security_context
    )
    agent.browser = mock_browser
    
    try:
        await agent.start()
    except Exception as e:
        pytest.fail(f"Failed to start ResearchAgent: {e}")
    yield agent

    try:
        await agent.stop()
    except Exception as e:
        print(f"Error stopping agent: {e}")
    finally:
        await agent.stop()


async def test_basic_research(research_agent, mock_browser):
    """Test basic research functionality."""
    # Mock browser responses
    mock_browser.browse.side_effect = [
        {
            'success': True,
            'url': 'https://search.test',
            'title': 'Search Results',
            'content': {
                'links': [
                    {
                        'text': 'Result 1',
                        'href': 'https://test1.com'
                    },
                    {
                        'text': 'Result 2',
                        'href': 'https://test2.com'
                    }
                ]
            }
        },
        {
            'success': True,
            'url': 'https://test1.com',
            'title': 'Test Page 1',
            'content': {
                'text': 'Test content 1',
                'links': []
            }
        },
        {
            'success': True,
            'url': 'https://test2.com',
            'title': 'Test Page 2',
            'content': {
                'text': 'Test content 2',
                'links': []
            }
        }
    ]
    result = await research_agent.execute_task({
        'action': 'start_research',
        'params': {
            'search_params': {
                'query': 'test query',
                'max_results': 2
            }
        }
    })
    assert result['success']
    assert len(result['results']) == 2

@pytest.mark.asyncio
async def test_research_session_continuation(research_agent, mock_browser):
    """Test continuing a research session."""
    # Initial research
    mock_browser.browse.side_effect = [
        {
            'success': True,
            'url': 'https://search.test',
            'title': 'Search Results',
            'content': {
                'links': [
                    {
                        'text': 'Result 1',
                        'href': 'https://test1.com'
                    }
                ]
            }
        },
        {
            'success': True,
            'url': 'https://test1.com',
            'title': 'Test Page 1',
            'content': {
                'text': 'Test content 1',
                'links': []
            }
        }
    ]
    
    initial_result = await research_agent.execute_task({
        'action': 'start_research',
        'params': {
            'search_params': {
                'query': 'test query',
                'max_results': 1
            }
        }
    })

    try:
        session_id = UUID(initial_result['session_id'])
    except ValueError:
        pytest.fail("Invalid session_id returned: not a valid UUID")
    
    # Continue research
    mock_browser.browse.side_effect = [
        {
            'success': True,
            'url': 'https://search.test/page2',
            'title': 'Search Results Page 2',
            'content': {
                'links': [
                    {
                        'text': 'Result 2',
                        'href': 'https://test2.com'
                    }
                ]
            }
        },
        {
            'success': True,
            'url': 'https://test2.com',
            'title': 'Test Page 2',
            'content': {
                'text': 'Test content 2',
                'links': []
            }
        }
    ]
    
    continued_result = await research_agent.continue_research(
        session_id,
        {'max_results': 1}
    )
    
    assert continued_result['success']
    assert len(continued_result['results']) == 1
    assert continued_result['session_id'] == str(session_id)

@pytest.mark.asyncio
async def test_research_summary(research_agent):
    """Test research results summarization."""
    # Create test session with results
    session_id = uuid4()
    session = ResearchSession(
        query='test query',  # Added required 'query' field
        search_params=SearchParams(query='test query'),
        results=[
            SearchResult(
                url='https://test1.com',
                title='Test 1',
                content={
                    'text': 'Test content 1',
                    'links': [{'href': 'link1'}, {'href': 'link2'}]
                }
            ),
            SearchResult(
                url='https://test2.com',
                title='Test 2',
                content={
                    'text': 'Test content 2',
                    'links': [{'href': 'link3'}]
                }
            )
        ],
        end_time=datetime.now(datetime.timezone.utc)
    )
    end_time = datetime.now(datetime.timezone.utc)
    research_agent.active_sessions[session_id] = session

    # Generate summary
    summary_result = await research_agent.execute_task({
        'action': 'summarize_results',
        'params': {
            'session_id': session_id
        }
    })

    assert summary_result['success']
    assert 'summary' in summary_result

@pytest.mark.asyncio
async def test_error_handling_during_research(mock_browser):
    """Test error handling during research."""
    # Simulate browser error
    mock_browser.browse.side_effect = Exception("Browser error")
    
    result = await research_agent.execute_task({
        'action': 'start_research',
        'parameters': {
            'search_params': {
                'query': 'test query'
            }
        }
    })
    
    assert not result['success']
    assert 'error' in result
    assert 'Browser error' in result['error']

@pytest.mark.asyncio
async def test_session_cleanup(research_agent):
    """Test session cleanup."""
    # Create test session
    session_id = uuid4()
    session = ResearchSession(
        query='test query',  # Added required 'query' field
        search_params=SearchParams(query='test query')
    )
    research_agent.active_sessions[session_id] = session
    
    try:
        await research_agent.cleanup_session(session_id)
        assert session_id not in research_agent.active_sessions
    except Exception as e:
        pytest.fail(f"Failed to clean up session: {e}")

@pytest.mark.asyncio
async def test_export_session(research_agent):
    """Test session export functionality."""
    # Create test session
    session_id = uuid4()
    session = ResearchSession(
        search_params=SearchParams(query='test query'),
        results=[
            SearchResult(
                url='https://test1.com',
                title='Test 1',
                content={'text': 'Test content'}
            )
        ],
        end_time=datetime.utcnow()
    )
    try:
        export_data = await research_agent.export_session(session_id)
    except Exception as e:
        pytest.fail(f"Failed to export session: {e}")
    
    assert 'session' in export_data
    assert 'summary' in export_data
    assert export_data['session']['search_params']['query'] == 'test query'

from ai_agent.agents.research.research_agent import ResearchAgent, SearchParams, SearchResult, ResearchSession
from ai_agent.core.message_bus import MessageBus
from ai_agent.core.security_manager import SecurityContext

@pytest.fixture
async def message_bus():
    bus = MessageBus("redis://localhost:6379")
    await bus.start()
    yield bus
    await bus.stop()

@pytest.fixture
def security_context():
    return SecurityContext(
        agent_id=uuid4(),
        permissions={"research.execute", "research.analyze"},
        auth_level=1
    )

@pytest.fixture
async def research_agent(message_bus, security_context):
    agent = ResearchAgent(
        agent_id=uuid4(),
        message_bus=message_bus,
        security_context=security_context
    )
    await agent.start()
    yield agent
    await agent.stop()

@pytest.mark.asyncio
async def test_perform_search(research_agent):
    """Test performing a web search."""
    params = {
        "query": "test query",
        "max_results": 5,
        "timeout": 30
    }
    
    try:
        result = await research_agent.execute_task({
            "action": "search",
            "params": params
        })
        
        assert result["success"] is True
        assert "session_id" in result
        assert "results" in result
        assert isinstance(result["results"], list)
        
        # Cleanup
        session_id = UUID(result["session_id"])
        await research_agent.cleanup_session(session_id)
        
    except Exception as e:
        pytest.fail(f"Test failed: {str(e)}")

@pytest.mark.asyncio
async def test_analyze_results(research_agent):
    """Test analyzing search results."""
    # First perform a search
    search_params = {
        "query": "test analysis",
        "max_results": 3,
        "timeout": 30
    }
    
    try:
        search_result = await research_agent.execute_task({
            "action": "search",
            "params": search_params
        })
        
        assert search_result["success"] is True
        session_id = UUID(search_result["session_id"])
        
        # Now analyze the results
        analysis_result = await research_agent.execute_task({
            "action": "analyze",
            "params": {
                "session_id": str(session_id)
            }
        })
        
        assert analysis_result["success"] is True
        assert "analysis" in analysis_result
        assert "query" in analysis_result["analysis"]
        assert "result_count" in analysis_result["analysis"]
        
        # Cleanup
        await research_agent.cleanup_session(session_id)
        
    except Exception as e:
        pytest.fail(f"Test failed: {str(e)}")

@pytest.mark.asyncio
async def test_invalid_action(research_agent):
    """Test handling of invalid action."""
    try:
        result = await research_agent.execute_task({
            "action": "invalid_action",
            "params": {}
        })
        
        assert result["success"] is False
        assert "error" in result
        
    except Exception as e:
        pytest.fail(f"Test failed: {str(e)}")
import pytest
from unittest.mock import Mock, patch
from uuid import UUID, uuid4
from datetime import datetime
