"""
Tests for the planning agent implementation.
"""

import pytest
from uuid import UUID, uuid4
from datetime import datetime, timedelta

from ai_agent.agents.planning.planning_agent import (
    PlanningAgent,
    ProjectPlan,
    PlannedTask,
    TaskDependency,
    ResourceRequirement,
    PlanningSession,
)
from ai_agent.core.message_bus import MessageBus
from ai_agent.core.security_manager import SecurityContext


@pytest.fixture
async def planning_agent():
    """Create a planning agent for testing."""
    agent_id = uuid4()
    message_bus = MessageBus("redis://localhost")
    security_context = SecurityContext(
        agent_id=agent_id,
        permissions={"planning.create", "planning.modify"},
        auth_level=0,
    )

    agent = PlanningAgent(
        agent_id=agent_id, message_bus=message_bus, security_context=security_context
    )

    await agent.start()
    yield agent
    await agent.stop()


@pytest.mark.asyncio
async def test_create_project_plan(planning_agent):
    """Test creating a basic project plan."""
    start_date = datetime.utcnow()
    end_date = start_date + timedelta(days=30)

    result = await planning_agent.execute_task(
        {
            "action": "create_plan",
            "parameters": {
                "title": "Test Project",
                "description": "Test project description",
                "start_date": start_date,
                "end_date": end_date,
                "initial_tasks": [
                    {
                        "title": "Task 1",
                        "description": "First task",
                        "estimated_hours": 8,
                        "required_capabilities": ["python"],
                    },
                    {
                        "title": "Task 2",
                        "description": "Second task",
                        "estimated_hours": 16,
                        "required_capabilities": ["python", "docker"],
                    },
                ],
            },
        }
    )

    assert result["success"]
    assert "session_id" in result
    assert len(result["plan"]["tasks"]) == 2


@pytest.mark.asyncio
async def test_decompose_task(planning_agent):
    """Test task decomposition."""
    task = PlannedTask(
        title="Complex Task",
        description="A complex task that needs decomposition",
        estimated_duration=timedelta(days=5),
        required_capabilities=["python", "docker", "kubernetes"],
        priority=1,
    )

    result = await planning_agent.execute_task(
        {"action": "decompose_task", "parameters": {"task": task.model_dump()}}
    )

    assert result["success"]
    assert len(result["subtasks"]) > 0
    assert result["complexity_score"] > 0.7


@pytest.mark.asyncio
async def test_update_project_plan(planning_agent):
    """Test updating a project plan."""
    # First create a plan
    start_date = datetime.utcnow()
    end_date = start_date + timedelta(days=30)

    create_result = await planning_agent.execute_task(
        {
            "action": "create_plan",
            "parameters": {
                "title": "Test Project",
                "description": "Test project description",
                "start_date": start_date,
                "end_date": end_date,
                "initial_tasks": [],
            },
        }
    )

    session_id = UUID(create_result["session_id"])

    # Update the plan
    update_result = await planning_agent.execute_task(
        {
            "action": "update_plan",
            "session_id": session_id,
            "parameters": {
                "add_tasks": [
                    {
                        "title": "New Task",
                        "description": "Added task",
                        "estimated_duration": timedelta(hours=8),
                        "required_capabilities": ["python"],
                    }
                ]
            },
        }
    )

    assert update_result["success"]
    assert len(update_result["plan"]["tasks"]) == 1


@pytest.mark.asyncio
async def test_analyze_dependencies(planning_agent):
    """Test dependency analysis."""
    # Create tasks with dependencies
    task1_id = uuid4()
    task2_id = uuid4()

    start_date = datetime.utcnow()
    end_date = start_date + timedelta(days=30)

    plan = ProjectPlan(
        title="Test Project",
        description="Test project with dependencies",
        start_date=start_date,
        end_date=end_date,
        tasks=[
            PlannedTask(
                id=task1_id,
                title="Task 1",
                description="First task",
                estimated_duration=timedelta(hours=8),
                start_time=start_date,
                end_time=start_date + timedelta(hours=8),
            ),
            PlannedTask(
                id=task2_id,
                title="Task 2",
                description="Second task",
                estimated_duration=timedelta(hours=8),
                dependencies=[TaskDependency(task_id=task1_id, type="finish_to_start")],
                start_time=start_date + timedelta(hours=8),
                end_time=start_date + timedelta(hours=16),
            ),
        ],
    )

    # Create session
    session_id = uuid4()
    planning_agent.active_sessions[session_id] = PlanningSession(project_plan=plan)

    result = await planning_agent.execute_task(
        {"action": "analyze_dependencies", "session_id": session_id}
    )

    assert result["success"]
    assert len(result["critical_path"]) == 2
    assert result["dependency_stats"]["total_dependencies"] == 1
    assert result["dependency_stats"]["max_depth"] == 1


@pytest.mark.asyncio
async def test_resource_conflict_detection(planning_agent):
    """Test detection of resource conflicts."""
    start_date = datetime.utcnow()

    # Create tasks with overlapping resource requirements
    task1_id = uuid4()
    task2_id = uuid4()

    plan = ProjectPlan(
        title="Test Project",
        description="Test project with resource conflicts",
        start_date=start_date,
        end_date=start_date + timedelta(days=30),
        tasks=[
            PlannedTask(
                id=task1_id,
                title="Task 1",
                description="First task",
                estimated_duration=timedelta(hours=8),
                start_time=start_date,
                end_time=start_date + timedelta(hours=8),
                resources=[
                    ResourceRequirement(type="developer", amount=1, units="person")
                ],
            ),
            PlannedTask(
                id=task2_id,
                title="Task 2",
                description="Second task",
                estimated_duration=timedelta(hours=8),
                start_time=start_date + timedelta(hours=4),
                end_time=start_date + timedelta(hours=12),
                resources=[
                    ResourceRequirement(type="developer", amount=1, units="person")
                ],
            ),
        ],
    )

    # Create session
    session_id = uuid4()
    planning_agent.active_sessions[session_id] = PlanningSession(project_plan=plan)

    result = await planning_agent.execute_task(
        {"action": "analyze_dependencies", "session_id": session_id}
    )

    assert result["success"]
    assert len(result["resource_conflicts"]) > 0


@pytest.mark.asyncio
async def test_export_plan(planning_agent):
    """Test plan export functionality."""
    # Create a simple plan
    start_date = datetime.utcnow()
    end_date = start_date + timedelta(days=30)

    plan = ProjectPlan(
        title="Export Test Project",
        description="Test project for export",
        start_date=start_date,
        end_date=end_date,
        tasks=[
            PlannedTask(
                title="Task 1",
                description="First task",
                estimated_duration=timedelta(hours=8),
                start_time=start_date,
                end_time=start_date + timedelta(hours=8),
            )
        ],
    )

    # Create session
    session_id = uuid4()
    planning_agent.active_sessions[session_id] = PlanningSession(project_plan=plan)

    export_result = await planning_agent.export_plan(session_id)

    assert "plan" in export_result
    assert "analysis" in export_result
    assert export_result["plan"]["title"] == "Export Test Project"


@pytest.mark.asyncio
async def test_session_cleanup(planning_agent):
    """Test planning session cleanup."""
    # Create a session
    session_id = uuid4()
    planning_agent.active_sessions[session_id] = PlanningSession(
        project_plan=ProjectPlan(
            title="Test Project",
            description="Test project",
            start_date=datetime.utcnow(),
            end_date=datetime.utcnow() + timedelta(days=30),
            tasks=[],
        )
    )

    # Clean up session
    await planning_agent.cleanup_session(session_id)
    assert session_id not in planning_agent.active_sessions


@pytest.mark.asyncio
async def test_optimize_schedule(planning_agent):
    """Test schedule optimization."""
    start_date = datetime.utcnow()
    task1_id = uuid4()
    task2_id = uuid4()
    task3_id = uuid4()

    plan = ProjectPlan(
        title="Optimization Test Project",
        description="Test project for schedule optimization",
        start_date=start_date,
        end_date=start_date + timedelta(days=30),
        tasks=[
            PlannedTask(
                id=task1_id,
                title="Task 1",
                description="First task",
                estimated_duration=timedelta(hours=8),
                priority=1,
            ),
            PlannedTask(
                id=task2_id,
                title="Task 2",
                description="Second task",
                estimated_duration=timedelta(hours=8),
                dependencies=[TaskDependency(task_id=task1_id, type="finish_to_start")],
                priority=2,
            ),
            PlannedTask(
                id=task3_id,
                title="Task 3",
                description="Third task",
                estimated_duration=timedelta(hours=8),
                dependencies=[TaskDependency(task_id=task2_id, type="finish_to_start")],
                priority=3,
            ),
        ],
    )

    # Create session
    session_id = uuid4()
    session = PlanningSession(project_plan=plan)
    planning_agent.active_sessions[session_id] = session

    # Optimize schedule
    await planning_agent._optimize_schedule(plan)

    # Verify task timings
    for task in plan.tasks:
        assert task.start_time is not None
        assert task.end_time is not None

    # Verify dependencies are respected
    task1 = next(t for t in plan.tasks if t.id == task1_id)
    task2 = next(t for t in plan.tasks if t.id == task2_id)
    task3 = next(t for t in plan.tasks if t.id == task3_id)

    assert task1.end_time <= task2.start_time
    assert task2.end_time <= task3.start_time


@pytest.mark.asyncio
async def test_resource_leveling(planning_agent):
    """Test resource leveling functionality."""
    start_date = datetime.utcnow()

    plan = ProjectPlan(
        title="Resource Test Project",
        description="Test project for resource leveling",
        start_date=start_date,
        end_date=start_date + timedelta(days=30),
        tasks=[
            PlannedTask(
                title="Task A",
                description="Task with developer resource",
                estimated_duration=timedelta(hours=8),
                resources=[
                    ResourceRequirement(type="developer", amount=1.0, units="person")
                ],
                start_time=start_date,
                end_time=start_date + timedelta(hours=8),
            ),
            PlannedTask(
                title="Task B",
                description="Another task with developer resource",
                estimated_duration=timedelta(hours=8),
                resources=[
                    ResourceRequirement(type="developer", amount=1.0, units="person")
                ],
                start_time=start_date,
                end_time=start_date + timedelta(hours=8),
            ),
        ],
    )

    # Create session
    session_id = uuid4()
    session = PlanningSession(project_plan=plan)
    planning_agent.active_sessions[session_id] = session

    # Level resources
    await planning_agent._level_resources(plan.tasks)

    # Verify tasks don't overlap in time for same resource
    tasks_by_resource = {}
    for task in plan.tasks:
        for resource in task.resources:
            if resource.type not in tasks_by_resource:
                tasks_by_resource[resource.type] = []
            tasks_by_resource[resource.type].append(task)

    for resource_tasks in tasks_by_resource.values():
        for i, task1 in enumerate(resource_tasks):
            for task2 in resource_tasks[i + 1 :]:
                # Check for no overlap
                assert (
                    task1.end_time <= task2.start_time
                    or task2.end_time <= task1.start_time
                )


@pytest.mark.asyncio
async def test_complex_dependency_analysis(planning_agent):
    """Test analysis of complex task dependencies."""
    start_date = datetime.utcnow()
    tasks = []

    # Create a chain of 5 tasks with various dependency types
    previous_id = None
    for i in range(5):
        task_id = uuid4()
        task = PlannedTask(
            id=task_id,
            title=f"Task {i+1}",
            description=f"Task {i+1} description",
            estimated_duration=timedelta(hours=8),
        )

        if previous_id:
            # Alternate between dependency types
            dep_type = "finish_to_start" if i % 2 == 0 else "start_to_start"
            task.dependencies.append(TaskDependency(task_id=previous_id, type=dep_type))

        tasks.append(task)
        previous_id = task_id

    plan = ProjectPlan(
        title="Complex Dependency Test",
        description="Test project with complex dependencies",
        start_date=start_date,
        end_date=start_date + timedelta(days=30),
        tasks=tasks,
    )

    # Create session
    session_id = uuid4()
    session = PlanningSession(project_plan=plan)
    planning_agent.active_sessions[session_id] = session

    # Analyze dependencies
    result = await planning_agent.execute_task(
        {"action": "analyze_dependencies", "session_id": session_id}
    )

    assert result["success"]
    assert len(result["critical_path"]) == 5  # All tasks should be critical
    assert result["dependency_stats"]["max_depth"] == 4  # Depth should be n-1
