Help me choose a stack and architect the following application:

I want an AI agent application that uses langchain and ollama with function calling capabilities.

It must support:
* function calling (maybe CrewAI?) with the following functions:
   * run any bash cli command
   * browse the web using chrome in non-headless mode so it doesn't get blocked by bot blockers that look for headless browser instances
   * The agent should run functions in a sandbox container
   * map host machine directories with the users permission.
Example:
```
You asked me to look in your ~/Documents/ directory for all files that contain information about your personal tax. 

Is it ok for me to map ~/Documents/ from the docker host into the sandbox container or would you prefer I copy the contents of ~/Documents/ into the sandbox instead?

Please select an action:
1) map
2) copy
3) deny
```
* Consider that I might need/want multiple agents that can work together to research, specify, architect, document, project manage, track and execute projects.