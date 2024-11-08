"""
Planning agent implementation for task decomposition and project management.
"""
from typing import Dict, Any, List, Optional
from uuid import UUID, uuid4
from datetime import datetime, timedelta
import logging
import asyncio

from pydantic import BaseModel, Field, field_validator
from typing_extensions import Literal

from ..base import BaseAgent
from ...core.message_bus import Message

logger = logging.getLogger(__name__)

class ResourceRequirement(BaseModel):
    """Resource requirement for a task."""
    type: str
    amount: float
    units: str
    priority: int = 0
    flexible: bool = False

class TaskDependency(BaseModel):
    """Task dependency definition."""
    task_id: UUID
    type: Literal["start_to_start", "start_to_finish", "finish_to_start", "finish_to_finish"]
    lag: timedelta = Field(default_factory=lambda: timedelta())

class PlannedTask(BaseModel):
    """Planned task definition."""
    id: UUID = Field(default_factory=uuid4)
    title: str
    description: str
    estimated_duration: timedelta
    required_capabilities: List[str] = Field(default_factory=list)
    dependencies: List[TaskDependency] = Field(default_factory=list)
    priority: int = 0
    status: str = "planned"
    assigned_agent: Optional[UUID] = None
    progress: float = 0.0
    start_time: Optional[datetime] = None
    end_time: Optional[datetime] = None
    resources: List[ResourceRequirement] = Field(default_factory=list)
    
    @field_validator('progress')
    def validate_progress(cls, v):
        """Validate progress is between 0 and 100."""
        if not 0 <= v <= 100:
            raise ValueError("Progress must be between 0 and 100")
        return v

class ProjectPlan(BaseModel):
    """Complete project plan."""
    id: UUID = Field(default_factory=uuid4)
    title: str
    description: str
    tasks: List[PlannedTask] = Field(default_factory=list)
    start_date: datetime
    end_date: datetime
    status: str = "draft"

class PlanningSession(BaseModel):
    """Active planning session."""
    id: UUID = Field(default_factory=uuid4)
    project_plan: ProjectPlan
    created_at: datetime = Field(default_factory=datetime.utcnow)
    updated_at: datetime = Field(default_factory=datetime.utcnow)
    status: str = "active"

class PlanningAgent(BaseAgent):
    """Agent for project planning and task decomposition."""
    
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.active_sessions: Dict[UUID, PlanningSession] = {}
        
    async def execute_task(self, task: Dict[str, Any]) -> Dict[str, Any]:
        """
        Execute a planning task.
        
        Args:
            task: Task definition containing:
                - action: Planning action to perform
                - parameters: Action parameters
                - session_id: Optional session ID for continued planning
                
        Returns:
            Dict containing planning results
        """
        try:
            action = task.get('action')
            parameters = task.get('parameters', {})
            session_id = task.get('session_id')
            
            if not action:
                raise ValueError("Missing required 'action' field")
                
            if action == 'create_plan':
                result = await self._create_project_plan(parameters)
            elif action == 'decompose_task':
                result = await self._decompose_task(parameters)
            elif action == 'update_plan':
                result = await self._update_project_plan(session_id, parameters)
            elif action == 'analyze_dependencies':
                result = await self._analyze_dependencies(session_id)
            else:
                raise ValueError(f"Unknown planning action: {action}")
                
            return {'success': True, **result}
                
        except Exception as e:
            error_msg = f"Planning task failed: {str(e)}"
            logger.error(error_msg)
            return {
                'success': False,
                'error': error_msg
            }
            
    async def _create_project_plan(
        self,
        parameters: Dict[str, Any]
    ) -> Dict[str, Any]:
        """Create a new project plan."""
        # Create initial plan
        plan = ProjectPlan(
            title=parameters['title'],
            description=parameters['description'],
            tasks=[],
            start_date=parameters['start_date'],
            end_date=parameters['end_date']
        )
        
        # Create planning session
        session = PlanningSession(project_plan=plan)
        self.active_sessions[session.id] = session
        
        # Decompose initial tasks if provided
        if 'initial_tasks' in parameters:
            for task_def in parameters['initial_tasks']:
                task = PlannedTask(
                    title=task_def['title'],
                    description=task_def['description'],
                    estimated_duration=timedelta(hours=task_def['estimated_hours']),
                    required_capabilities=task_def.get('required_capabilities', []),
                    priority=task_def.get('priority', 0)
                )
                plan.tasks.append(task)
                
        # Analyze and optimize initial plan
        await self._optimize_plan(session.id)
        
        return {
            'success': True,
            'session_id': str(session.id),
            'plan': plan.model_dump()
        }
        
    async def _decompose_task(
        self,
        parameters: Dict[str, Any]
    ) -> Dict[str, Any]:
        """Decompose a task into subtasks."""
        parent_task = PlannedTask(**parameters['task'])
        
        # Analyze task complexity and requirements
        complexity_score = await self._analyze_task_complexity(parent_task)
        
        subtasks = []
        if complexity_score > 0.7:  # Complex task
            # Break down into smaller tasks
            components = await self._identify_task_components(parent_task)
            
            for component in components:
                subtask = PlannedTask(
                    title=f"{parent_task.title} - {component['name']}",
                    description=component['description'],
                    estimated_duration=timedelta(hours=component['estimated_hours']),
                    required_capabilities=component['required_capabilities'],
                    priority=parent_task.priority
                )
                
                # Add dependency on previous subtask if sequential
                if subtasks and component.get('sequential', True):
                    subtask.dependencies.append(
                        TaskDependency(
                            task_id=subtasks[-1].id,
                            type="finish_to_start"
                        )
                    )
                        
                subtasks.append(subtask)
                
        return {
            'success': True,
            'parent_task': parent_task.model_dump(),
            'subtasks': [task.model_dump() for task in subtasks],
            'complexity_score': complexity_score
        }
        
    async def _update_project_plan(
        self,
        session_id: UUID,
        parameters: Dict[str, Any]
    ) -> Dict[str, Any]:
        """Update an existing project plan."""
        session = self.active_sessions.get(session_id)
        if not session:
            raise ValueError(f"Session {session_id} not found")
            
        plan = session.project_plan
        
        # Apply updates
        if 'add_tasks' in parameters:
            for task_def in parameters['add_tasks']:
                task = PlannedTask(**task_def)
                plan.tasks.append(task)
                
        if 'update_tasks' in parameters:
            for task_update in parameters['update_tasks']:
                task_id = UUID(task_update['id'])
                task = next((t for t in plan.tasks if t.id == task_id), None)
                if task:
                    for key, value in task_update.items():
                        if key != 'id':
                            setattr(task, key, value)
                            
        if 'remove_tasks' in parameters:
            task_ids = set(UUID(tid) for tid in parameters['remove_tasks'])
            plan.tasks = [t for t in plan.tasks if t.id not in task_ids]
            
        # Update plan dates if needed
        if plan.tasks:
            plan.start_date = min(
                t.start_time or plan.start_date
                for t in plan.tasks
            )
            plan.end_date = max(
                (t.end_time or plan.end_date)
                for t in plan.tasks
            )
            
        # Re-optimize plan
        await self._optimize_plan(session_id)
        
        session.updated_at = datetime.utcnow()
        
        return {
            'success': True,
            'plan': plan.model_dump()
        }
        
    async def _analyze_dependencies(
        self,
        session_id: UUID
    ) -> Dict[str, Any]:
        """Analyze task dependencies and constraints."""
        try:
            session = self.active_sessions.get(session_id)
            if not session:
                raise ValueError(f"Session {session_id} not found")
                
            plan = session.project_plan
            
            # Build dependency graph
            dependency_graph = {}
            task_map = {task.id: task for task in plan.tasks}
            
            # First pass - create nodes
            for task in plan.tasks:
                dependency_graph[task.id] = {
                    'task': task,
                    'dependencies': set(),
                    'dependents': set()
                }
                
            # Second pass - add dependencies
            for task in plan.tasks:
                task_id = task.id
                for dep in task.dependencies:
                    dep_id = dep.task_id
                    if dep_id not in dependency_graph:
                        raise ValueError(f"Referenced task {dep_id} not found")
                    dependency_graph[task_id]['dependencies'].add(dep_id)
                    dependency_graph[dep_id]['dependents'].add(task_id)
                    
            # Find critical path
            critical_path = await self._find_critical_path(dependency_graph)
            
            # Analyze resource constraints
            resource_conflicts = await self._analyze_resource_conflicts(plan.tasks)
            
            return {
                'success': True,
                'critical_path': [str(task_id) for task_id in critical_path],
                'resource_conflicts': resource_conflicts,
                'dependency_stats': {
                    'total_dependencies': sum(
                        len(node['dependencies'])
                        for node in dependency_graph.values()
                    ),
                    'max_depth': await self._calculate_max_depth(dependency_graph)
                }
            }
            
        except Exception as e:
            logger.error(f"Dependency analysis failed: {str(e)}")
            return {
                'success': False,
                'error': str(e)
            }
            
    async def _optimize_plan(self, session_id: UUID):
        """Optimize project plan timing and resources."""
        session = self.active_sessions.get(session_id)
        if not session:
            return
            
        plan = session.project_plan
        
        # Level resources
        await self._level_resources(plan.tasks)
        
        # Optimize task scheduling
        await self._optimize_schedule(plan)
        
    async def _analyze_task_complexity(self, task: PlannedTask) -> float:
        """Analyze task complexity (0.0 to 1.0)."""
        factors = [
            len(task.required_capabilities) * 0.2,
            len(task.resources) * 0.15,
            (task.estimated_duration.total_seconds() / (8 * 3600)) * 0.3,  # Normalize to 8-hour days
            len(task.description.split()) * 0.01  # Description length factor
        ]
        return min(1.0, sum(factors))
        
    async def _identify_task_components(
        self,
        task: PlannedTask
    ) -> List[Dict[str, Any]]:
        """Identify logical components of a task."""
        # This is a simplified version - in practice, would use LLM
        # to analyze task description and identify components
        components = [
            {
                'name': 'Research',
                'description': f"Research phase for {task.title}",
                'estimated_hours': task.estimated_duration.total_seconds() / 3600 * 0.2,
                'required_capabilities': ['research'],
                'sequential': True
            },
            {
                'name': 'Implementation',
                'description': f"Implementation phase for {task.title}",
                'estimated_hours': task.estimated_duration.total_seconds() / 3600 * 0.6,
                'required_capabilities': task.required_capabilities,
                'sequential': True
            },
            {
                'name': 'Testing',
                'description': f"Testing phase for {task.title}",
                'estimated_hours': task.estimated_duration.total_seconds() / 3600 * 0.2,
                'required_capabilities': ['testing'],
                'sequential': True
            }
        ]
        
        return components
        
    async def _find_critical_path(
        self,
        dependency_graph: Dict[UUID, Dict]
    ) -> List[UUID]:
        """Find the critical path through tasks."""
        # Perform forward pass
        earliest_start = {}
        task_durations = {}
        for task_id in self._topological_sort(dependency_graph):
            node = dependency_graph[task_id]
            task = node['task']
            task_duration = task.estimated_duration.total_seconds()
            task_durations[task_id] = task_duration
            
            earliest = 0
            for dep_id in node['dependencies']:
                dep_task = dependency_graph[dep_id]['task']
                dep_end = earliest_start[dep_id] + \
                    dep_task.estimated_duration.total_seconds()
                earliest = max(earliest, dep_end)
                
            earliest_start[task_id] = earliest
            
        # Perform backward pass
        latest_start = {}
        max_duration = max(earliest_start[tid] + task_durations[tid] for tid in earliest_start)
        critical_path = []
        
        for task_id in reversed(list(dependency_graph.keys())):
            node = dependency_graph[task_id]
            task = node['task']
            
            if not node['dependents']:
                latest_start[task_id] = earliest_start[task_id]
            else:
                latest = min(
                    latest_start[dep_id] - task_durations[task_id]
                    for dep_id in node['dependents']
                )
                latest_start[task_id] = latest
                
            if earliest_start[task_id] == latest_start[task_id]:
                critical_path.append(task_id)
                
        return list(reversed(critical_path))
        
    async def _level_resources(self, tasks: List[PlannedTask]):
        """Level resource usage across tasks."""
        # Group tasks by resource type
        resource_groups = {}
        for task in tasks:
            for resource in task.resources:
                if resource.type not in resource_groups:
                    resource_groups[resource.type] = []
                resource_groups[resource.type].append(task)
                
        # Level each resource type
        for resource_type, resource_tasks in resource_groups.items():
            # Sort tasks by priority and dependencies
            sorted_tasks = sorted(
                resource_tasks,
                key=lambda t: (-t.priority, len(t.dependencies))
            )
            
            # Allocate resources
            allocation = {}
            for task in sorted_tasks:
                # Find available time slot
                slot_found = False
                current_time = task.start_time or task.end_time or datetime.utcnow()
                
                while not slot_found:
                    # Check if resources available
                    conflicts = False
                    for other_id in allocation.keys():
                        if (
                            allocation[other_id][0] <= current_time <
                            allocation[other_id][1]
                        ):
                            conflicts = True
                            current_time = allocation[other_id][1]
                            break
                            
                    if not conflicts:
                        allocation[task.id] = (
                            current_time,
                            current_time + task.estimated_duration
                        )
                        task.start_time = current_time
                        task.end_time = current_time + task.estimated_duration
                        slot_found = True
                        
    async def _optimize_schedule(self, plan: ProjectPlan):
        """Optimize task scheduling within project constraints."""
        try:
            # Sort tasks by dependencies
            sorted_tasks = self._topological_sort_tasks(plan.tasks)
            
            # Forward pass - earliest start times
            task_times = {}  # Store calculated times before updating tasks
            
            for task in sorted_tasks:
                earliest_start = plan.start_date
                
                # Consider dependencies
                for dep in task.dependencies:
                    dep_task = next(
                        (t for t in plan.tasks if t.id == dep.task_id),
                        None
                    )
                    if dep_task and dep_task.id in task_times:
                        dep_start, dep_end = task_times[dep_task.id]
                        if dep.type == "finish_to_start":
                            earliest_start = max(
                                earliest_start,
                                dep_end + dep.lag
                            )
                        elif dep.type == "start_to_start":
                            earliest_start = max(
                                earliest_start,
                                dep_start + dep.lag
                            )
                        elif dep.type == "finish_to_finish":
                            earliest_start = max(
                                earliest_start,
                                dep_end - task.estimated_duration + dep.lag
                            )
                        elif dep.type == "start_to_finish":
                            earliest_start = max(
                                earliest_start,
                                dep_start - task.estimated_duration + dep.lag
                            )
                        else:
                            raise ValueError(f"Unsupported dependency type: {dep.type}")

                # Calculate task times
                task_end = earliest_start + task.estimated_duration
                task_times[task.id] = (earliest_start, task_end)
                
                # Update task times immediately
                task.start_time = earliest_start
                task.end_time = task_end
                
            # Update all tasks with calculated times
            for task in plan.tasks:
                if task.id in task_times:
                    task.start_time, task.end_time = task_times[task.id]
                    
        except Exception as e:
            logger.error(f"Failed to optimize schedule: {e}")
            
    def _topological_sort_tasks(self, tasks: List[PlannedTask]) -> List[PlannedTask]:
        """Sort tasks based on dependencies."""
        # Build dependency graph
        graph = {task.id: {'task': task, 'dependencies': set(), 'dependents': set()} 
                for task in tasks}
                
        # Add dependencies
        for task in tasks:
            for dep in task.dependencies:
                graph[task.id]['dependencies'].add(dep.task_id)
                graph[dep.task_id]['dependents'].add(task.id)
                
        # Perform topological sort
        sorted_ids = self._topological_sort(graph)
        return [graph[task_id]['task'] for task_id in sorted_ids]
        
    def _topological_sort(
        self,
        graph: Dict[UUID, Dict]
    ) -> List[UUID]:
        """Perform topological sort on dependency graph."""
        in_degree = {node: 0 for node in graph}
        for node in graph:
            for dep in graph[node]['dependencies']:
                in_degree[node] += 1  # Corrected line
                
        queue = [node for node in graph if in_degree[node] == 0]
        sorted_nodes = []
        
        while queue:
            node = queue.pop(0)
            sorted_nodes.append(node)
            
            for neighbor in graph[node]['dependents']:
                in_degree[neighbor] -= 1
                if in_degree[neighbor] == 0:
                    queue.append(neighbor)
                    
        if len(sorted_nodes) != len(graph):
            raise ValueError("Graph has cycles")
        
        return sorted_nodes
        
    async def _calculate_max_depth(
        self,
        dependency_graph: Dict[UUID, Dict]
    ) -> int:
        """Calculate maximum dependency depth."""
        depths = {}
        
        def calculate_depth(node_id: UUID) -> int:
            if node_id in depths:
                return depths[node_id]
                
            if not dependency_graph[node_id]['dependencies']:
                depths[node_id] = 0
                return 0
                
            max_dep_depth = max(
                calculate_depth(dep_id)
                for dep_id in dependency_graph[node_id]['dependencies']
            )
            
            depths[node_id] = max_dep_depth + 1
            return depths[node_id]
            
        return max(
            calculate_depth(node_id)
            for node_id in dependency_graph
        )
        
    async def _analyze_resource_conflicts(
        self,
        tasks: List[PlannedTask]
    ) -> List[Dict[str, Any]]:
        """Analyze resource conflicts between tasks."""
        conflicts = []
        
        # Group tasks by resource type
        resource_usage = {}
        for task in tasks:
            if not task.start_time or not task.end_time:
                continue
                
            for resource in task.resources:
                if resource.type not in resource_usage:
                    resource_usage[resource.type] = []
                    
                resource_usage[resource.type].append({
                    'task_id': task.id,
                    'start': task.start_time,
                    'end': task.end_time,
                    'amount': resource.amount
                })
                
        # Check for overlapping usage
        for resource_type, usages in resource_usage.items():
            # Sort by start time
            sorted_usage = sorted(usages, key=lambda x: x['start'])
            
            # Check each pair of overlapping tasks
            for i in range(len(sorted_usage)):
                for j in range(i + 1, len(sorted_usage)):
                    if sorted_usage[i]['end'] > sorted_usage[j]['start']:
                        conflicts.append({
                            'resource_type': resource_type,
                            'task1_id': sorted_usage[i]['task_id'],
                            'task2_id': sorted_usage[j]['task_id'],
                            'start': sorted_usage[j]['start'],
                            'end': min(
                                sorted_usage[i]['end'],
                                sorted_usage[j]['end']
                            ),
                            'total_amount': (
                                sorted_usage[i]['amount'] +
                                sorted_usage[j]['amount']
                            )
                        })
                        
        return conflicts
        
    async def export_plan(
        self,
        session_id: UUID,
        format: str = 'json'
    ) -> Dict[str, Any]:
        """Export project plan in specified format."""
        session = self.active_sessions.get(session_id)
        if not session:
            raise ValueError(f"Session {session_id} not found")
            
        if format == 'json':
            return {
                'plan': session.project_plan.model_dump(),
                'analysis': await self._analyze_dependencies(session_id)
            }
        else:
            raise ValueError(f"Unsupported export format: {format}")
            
    async def cleanup_session(self, session_id: UUID):
        """Clean up a planning session."""
        if session_id in self.active_sessions:
            del self.active_sessions[session_id]
