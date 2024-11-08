# AI Agent System Specification

## Part 2: Sandbox and Component Implementation

### 1. Container Management

#### 1.1 Base Container Configuration

```dockerfile
# Base Agent Container
FROM python:3.11-slim

# System dependencies
RUN apt-get update && apt-get install -y \
    chromium \
    chromium-driver \
    xvfb \
    curl \
    bash \
    && rm -rf /var/lib/apt/lists/*

# Python dependencies
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

# Security configurations
RUN useradd -m -s /bin/bash agent
USER agent

# Configure environment
ENV PYTHONUNBUFFERED=1
ENV SANDBOX_MODE=1

WORKDIR /app
```

#### 1.2 Container Manager

```python
class ContainerManager:
    def __init__(self):
        self.client = docker.from_client()
        self.active_containers: Dict[UUID, Container] = {}

    async def create_agent_container(
        self,
        agent_type: AgentType,
        security_context: SecurityContext
    ) -> Container:
        config = ContainerConfig(
            image="ai-agent-base:latest",
            command=["python", "-m", "agent.run"],
            environment={
                "AGENT_TYPE": agent_type.value,
                "SECURITY_CONTEXT": security_context.json(),
                "MESSAGE_BUS_URL": self.message_bus_url
            },
            mounts=self._get_allowed_mounts(security_context),
            resources=ResourceConfig(
                cpu_limit="1.0",
                memory_limit="1g",
                read_only_root=True
            ),
            network_config=NetworkConfig(
                isolated=True,
                allowed_hosts=["message-bus", "ollama"]
            )
        )

        container = await self.client.containers.create(config)
        self.active_containers[container.id] = container
        return container

    async def cleanup(self, container_id: UUID):
        if container := self.active_containers.get(container_id):
            await container.stop(timeout=10)
            await container.remove()
            del self.active_containers[container_id]
```

### 2. Agent Implementation

#### 2.1 Base Agent Class

```python
class BaseAgent:
    def __init__(
        self,
        agent_id: UUID,
        security_context: SecurityContext,
        message_bus: MessageBus
    ):
        self.agent_id = agent_id
        self.security_context = security_context
        self.message_bus = message_bus
        self.function_registry = FunctionRegistry()
        self.state = AgentState()

    async def initialize(self):
        await self.message_bus.subscribe(
            f"agent.{self.agent_id}",
            self.handle_message
        )

    async def handle_message(self, message: Message):
        try:
            handler = self.get_message_handler(message.type)
            response = await handler(message)
            await self.message_bus.publish(
                message.reply_channel,
                response
            )
        except Exception as e:
            await self.handle_error(e, message)

    @abstractmethod
    async def execute_task(self, task: Task) -> TaskResult:
        pass
```

#### 2.2 Specialized Agents

##### 2.2.1 Research Agent

```python
class ResearchAgent(BaseAgent):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.browser = SecureBrowser()

    async def execute_task(self, task: Task) -> TaskResult:
        match task.type:
            case TaskType.WEB_SEARCH:
                return await self.perform_web_search(task)
            case TaskType.CONTENT_ANALYSIS:
                return await self.analyze_content(task)
            case _:
                raise UnsupportedTaskError(task.type)

    async def perform_web_search(self, task: WebSearchTask):
        urls = await self.browser.search(task.query)
        results = []

        for url in urls:
            content = await self.browser.extract_content(url)
            results.append(SearchResult(url=url, content=content))

        return WebSearchResult(results=results)
```

##### 2.2.2 Execution Agent

```python
class ExecutionAgent(BaseAgent):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.bash = BashExecutor()

    async def execute_task(self, task: Task) -> TaskResult:
        match task.type:
            case TaskType.COMMAND_EXECUTION:
                return await self.execute_command(task)
            case TaskType.FILE_OPERATION:
                return await self.handle_file_operation(task)
            case _:
                raise UnsupportedTaskError(task.type)

    async def execute_command(self, task: CommandTask):
        if not self.security_context.can_execute(task.command):
            raise PermissionError(f"Cannot execute: {task.command}")

        result = await self.bash.execute(
            command=task.command,
            timeout=task.timeout,
            env=task.environment
        )
        return CommandResult(
            exit_code=result.exit_code,
            stdout=result.stdout,
            stderr=result.stderr
        )
```

### 3. Function Registry and Execution

#### 3.1 Function Registry

```python
class Function(BaseModel):
    name: str
    description: str
    parameters: Dict[str, ParameterSpec]
    required_permissions: Set[Permission]
    resource_requirements: ResourceRequirements

class FunctionRegistry:
    def __init__(self):
        self.functions: Dict[str, Function] = {}

    def register(self, function: Function):
        self.functions[function.name] = function

    async def execute(
        self,
        name: str,
        parameters: Dict[str, Any],
        security_context: SecurityContext
    ) -> Any:
        function = self.functions[name]

        # Validate permissions
        if not security_context.has_permissions(function.required_permissions):
            raise PermissionError(f"Missing permissions for {name}")

        # Validate parameters
        validated_params = self.validate_parameters(function, parameters)

        # Execute function
        return await function.execute(**validated_params)
```

#### 3.2 Built-in Functions

```python
@register_function
class BashFunction(Function):
    name = "bash.execute"
    description = "Execute a bash command"
    parameters = {
        "command": ParameterSpec(type=str),
        "timeout": ParameterSpec(type=int, default=30),
        "env": ParameterSpec(type=Dict[str, str], default_factory=dict)
    }
    required_permissions = {Permission.EXECUTE_COMMAND}

@register_function
class BrowseFunction(Function):
    name = "web.browse"
    description = "Browse a web page"
    parameters = {
        "url": ParameterSpec(type=str),
        "javascript_enabled": ParameterSpec(type=bool, default=True),
        "timeout": ParameterSpec(type=int, default=30)
    }
    required_permissions = {Permission.WEB_ACCESS}
```

### 4. Web Browser Implementation

#### 4.1 Secure Browser

```python
class SecureBrowser:
    def __init__(self):
        self.options = uc.ChromeOptions()
        self._configure_browser()

    def _configure_browser(self):
        self.options.add_argument('--no-sandbox')
        self.options.add_argument('--disable-dev-shm-usage')
        self.options.add_argument('--disable-gpu')
        self.options.add_argument('--window-size=1920,1080')

        # Anti-detection measures
        self.options.add_argument('--disable-blink-features=AutomationControlled')
        self.options.add_experimental_option("excludeSwitches", ["enable-automation"])
        self.options.add_experimental_option('useAutomationExtension', False)

    async def browse(self, url: str, **kwargs) -> WebPage:
        async with self._create_session() as session:
            page = await session.new_page()

            # Set realistic user behavior
            await self._configure_realistic_behavior(page)

            # Navigate with retry logic
            content = await self._safe_navigate(page, url)

            return WebPage(
                url=url,
                content=content,
                screenshots=await self._capture_screenshots(page)
            )

    async def _configure_realistic_behavior(self, page):
        # Randomized typing speed
        await page.setDefaultNavigationTimeout(30000)
        await page.keyboard.setDelay({'min': 50, 'max': 100})

        # Human-like mouse movements
        await page.mouse.move(
            x=random.randint(0, 800),
            y=random.randint(0, 600)
        )
```

### 5. File System Access Management

#### 5.1 File System Manager

```python
class FileSystemManager:
    def __init__(self, security_context: SecurityContext):
        self.security_context = security_context
        self.allowed_paths: Set[Path] = set()

    async def request_access(
        self,
        host_path: Path,
        access_type: AccessType
    ) -> bool:
        request = FileAccessRequest(
            path=host_path,
            access_type=access_type,
            agent_id=self.security_context.agent_id
        )

        # Request user permission
        approved = await self.request_user_permission(request)
        if not approved:
            return False

        # Setup access based on type
        match access_type:
            case AccessType.MAP:
                await self._setup_bind_mount(host_path)
            case AccessType.COPY:
                await self._copy_to_container(host_path)

        self.allowed_paths.add(host_path)
        return True

    async def _setup_bind_mount(self, host_path: Path):
        mount_point = Path("/mnt/host") / host_path.name

        mount_config = BindMount(
            source=host_path,
            target=mount_point,
            read_only=True
        )

        await self.container_manager.add_mount(
            self.security_context.container_id,
            mount_config
        )
```

