# ai_agent/agents/execution/bash_agent.py

"""
Bash execution agent implementation.
Provides secure bash command execution in a sandboxed environment.
"""
from typing import Dict, Any, Optional
from uuid import UUID, uuid4
import logging
import asyncio
import os
import signal
from datetime import datetime

from pydantic import BaseModel, Field, ConfigDict

from ...core.message_bus import Message
from ...core.security_manager import SecurityOperation
from ..base import BaseAgent

logger = logging.getLogger(__name__)

class CommandResult(BaseModel):
    """Result of a command execution."""
    exit_code: int
    stdout: str
    stderr: str
    duration: float
    start_time: datetime
    end_time: datetime
    command: str
    environment: Dict[str, str]
    terminated: bool = False  # New field to track if command was terminated

class CommandExecution(BaseModel):
    """Active command execution tracking."""
    model_config = ConfigDict(arbitrary_types_allowed=True)  # Moved before fields
    process: Optional[asyncio.subprocess.Process] = None
    start_time: datetime = Field(default_factory=datetime.utcnow)
    command: str
    timeout: Optional[float] = None
    environment: Dict[str, str] = Field(default_factory=dict)
    terminated: bool = False  # Flag to indicate termination

class BashExecutionAgent(BaseAgent):
    """Agent for executing bash commands."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.active_executions: Dict[UUID, CommandExecution] = {}
        self._setup_environment()
        
    def _setup_environment(self):
        """Set up the execution environment."""
        self.base_environment = {
            'PATH': os.environ.get('PATH', ''),
            'HOME': '/home/agent',
            'SHELL': '/bin/bash',
            'LANG': 'en_US.UTF-8',
            'TERM': 'xterm-256color',
        }
        
    async def execute_task(self, task: Dict[str, Any]) -> Dict[str, Any]:
        command = task.get('command')
        timeout = task.get('timeout', 300)  # Default 5 minute timeout
        env = task.get('env', {})

        if not command:
            return {
                'success': False,
                'result': {
                    'exit_code': -1,
                    'stdout': "",
                    'stderr': "No command specified",
                    'duration': 0.0,
                    'start_time': datetime.utcnow(),
                    'end_time': datetime.utcnow(),
                    'command': "",
                    'environment': env,
                    'terminated': False
                }
            }
            
        operation = SecurityOperation(
            operation_type='execute_command',
            resource='bash',
            required_permissions={'bash.execute'}
        )
        if not await self.security_context.validate_operation(
            self.security_context.context_id,
            operation
        ):
            return {
                'success': False,
                'result': {
                    'exit_code': -1,
                    'stdout': "",
                    'stderr': "Command execution not permitted",
                    'duration': 0.0,
                    'start_time': datetime.utcnow(),
                    'end_time': datetime.utcnow(),
                    'command': command,
                    'environment': env,
                    'terminated': False
                }
            }
            
        execution_id = uuid4()
        try:
            result = await self._execute_command(command, timeout, env, execution_id)
            success = result.exit_code == 0 and not result.terminated
            return {
                'success': success,
                'result': result.model_dump()
            }
                
        except asyncio.TimeoutError:
            return {
                'success': False,
                'result': {
                    'exit_code': -1,
                    'stdout': "",
                    'stderr': f"Command timed out after {timeout} seconds",
                    'duration': 0.0,
                    'start_time': datetime.utcnow(),
                    'end_time': datetime.utcnow(),
                    'command': command,
                    'environment': env,
                    'terminated': True
                }
            }
        except Exception as e:
            return {
                'success': False,
                'result': {
                    'exit_code': -1,
                    'stdout': "",
                    'stderr': f"Command execution failed: {str(e)}",
                    'duration': 0.0,
                    'start_time': datetime.utcnow(),
                    'end_time': datetime.utcnow(),
                    'command': command,
                    'environment': env,
                    'terminated': True
                }
            }
            
    async def _execute_command(self, command: str, timeout: Optional[float] = None, env: Optional[Dict[str, str]] = None, execution_id: Optional[UUID] = None) -> CommandResult:
        execution_env = self.base_environment.copy()
        if env:
            execution_env.update(env)
            
        start_time = datetime.utcnow()
        if not execution_id:
            execution_id = uuid4()
        terminated = False  # Track termination status
        
        try:
            process = await asyncio.create_subprocess_shell(
                command,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                env=execution_env,
                start_new_session=True
            )
            
            execution = CommandExecution(
                process=process,
                start_time=start_time,
                command=command,
                timeout=timeout,
                environment=execution_env
            )
            self.active_executions[execution_id] = execution
            
            try:
                stdout, stderr = await asyncio.wait_for(process.communicate(), timeout=timeout)
            except asyncio.CancelledError:
                # Handle cancellation due to stop()
                terminated = True
                raise
            except asyncio.TimeoutError:
                # Handle timeout
                terminated = True
                raise
            else:
                terminated = execution.terminated  # Check if terminated during execution
            end_time = datetime.utcnow()
            duration = (end_time - start_time).total_seconds()

            logger.debug(f"Command '{command}' terminated: {terminated}, returncode: {process.returncode}")

            return CommandResult(
                exit_code=process.returncode,
                stdout=stdout.decode('utf-8', errors='replace').strip(),
                stderr=stderr.decode('utf-8', errors='replace').strip(),
                duration=duration,
                start_time=start_time,
                end_time=end_time,
                command=command,
                environment=execution_env,
                terminated=terminated
            )
                
        except asyncio.TimeoutError:
            if process and process.pid:
                pgid = os.getpgid(process.pid)
                os.killpg(pgid, signal.SIGTERM)
                await process.wait()
                terminated = True
                logger.debug(f"Command '{command}' timed out and was terminated.")
            return CommandResult(
                exit_code=-1,
                stdout="",
                stderr="Process terminated due to timeout",
                duration=(datetime.utcnow() - start_time).total_seconds(),
                start_time=start_time,
                end_time=datetime.utcnow(),
                command=command,
                environment=execution_env,
                terminated=terminated
            )
        except asyncio.CancelledError:
            if process and process.pid:
                pgid = os.getpgid(process.pid)
                os.killpg(pgid, signal.SIGTERM)
                await process.wait()
                terminated = True
                logger.debug(f"Command '{command}' was cancelled and terminated.")
            raise
        except Exception as e:
            end_time = datetime.utcnow()
            duration = (end_time - start_time).total_seconds()
            logger.error(f"Exception during command execution: {e}")
            return CommandResult(
                exit_code=-1,
                stdout="",
                stderr=str(e),
                duration=duration,
                start_time=start_time,
                end_time=end_time,
                command=command,
                environment=execution_env,
                terminated=terminated
            )
        finally:
            if execution_id in self.active_executions:
                del self.active_executions[execution_id]
                
    async def stop(self):
        """Stop all currently active executions."""
        for execution_id, execution in list(self.active_executions.items()):
            process = execution.process
            if process and process.pid:
                try:
                    pgid = os.getpgid(process.pid)
                    os.killpg(pgid, signal.SIGTERM)
                    execution.terminated = True  # Set terminated flag
                    try:
                        await asyncio.wait_for(process.wait(), timeout=5)
                        logger.debug(f"Process {process.pid} terminated with SIGTERM.")
                    except asyncio.TimeoutError:
                        os.killpg(pgid, signal.SIGKILL)
                        await process.wait()
                        logger.debug(f"Process {process.pid} killed with SIGKILL.")
                except Exception as e:
                    logger.error(f"Failed to terminate process {process.pid}: {str(e)}")
            execution.terminated = True  # Ensure terminated flag is set
            # Check if execution_id is still in active_executions before deleting
            if execution_id in self.active_executions:
                del self.active_executions[execution_id]
