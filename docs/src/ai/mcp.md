# Model Context Protocol

> Tip for parallel tool workflows: for multitasking and parallel execution, prefer using the async terminal tool `enhanced_terminal_async`. It starts commands in the background and returns a `job_id` promptly so the model can continue using other tools (e.g., MCP tools) in parallel. Use `enhanced_terminal_job_status` to poll/update UI state and optionally fetch `full_output`, and `enhanced_terminal_list_jobs` to enumerate active jobs.

## Integrating a Business-Domain Context MCP Server

You can integrate a business-domain context server (for example, `mcp-server-business-domain-context`) to ground the LLM in your organization’s concepts, entities, and terminology. This server’s tools can be combined with parallel terminal jobs for high-throughput pipelines (e.g., context retrieval + concurrent builds/tests).

### Install as a Custom Server

Add it to your `settings.json`:

```json [settings]
{
  "context_servers": {
    "business-domain": {
      "source": "custom",
      "command": "mcp-server-business-domain-context",
      "args": [],
      "env": {
        "BUSINESS_DOMAIN_API_KEY": "…",
        "BUSINESS_DOMAIN_ENDPOINT": "https://api.example.com"
      }
    }
  }
}
```

- Replace the environment variables with those required by your server.
- Ensure the command is on your PATH or specify an absolute path.

### Enable the Server’s Tools in a Profile

Create or adjust a profile to enable only the tools you need from the business-domain server, and include the async terminal tools for parallel execution:

```json [settings]
"agent": {
  "profiles": {
    "business-domain": {
      "name": "Business Domain",
      "enable_all_context_servers": false,
      "context_servers": {
        "business-domain": {
          "tools": {
            "domain_concepts_search": true,
            "domain_entity_lookup": true,
            "domain_context_summarize": true
          }
        }
      },
      "tools": {
        "enhanced_terminal_async": true,
        "enhanced_terminal_job_status": true,
        "enhanced_terminal_list_jobs": true
      }
    }
  }
}
```

- This isolates domain tools to avoid conflicts and ensures the model can use async terminal tools for concurrency.

### Recommended Pipeline Pattern

1. Ground context with business-domain tools:
   - Call `domain_concepts_search` for key terms, taxonomy, or related entities.
   - Call `domain_entity_lookup` for canonical definitions, IDs, or constraints.
   - Optionally `domain_context_summarize` to condense retrieved context for the next steps.

2. Launch parallel terminal work with `enhanced_terminal_async`:
   - Start long-running tasks (e.g., builds, tests, migrations) with `timeout_seconds: 0` to immediately get a `job_id` and continue.
   - Whitelist only required environment variables via `env_whitelist` for safer execution.

3. Poll job progress with `enhanced_terminal_job_status`:
   - Periodically check `state`, `runtime_secs`, `preview`, and `exit_code`.
   - Request `full_output: true` after `state` is `finished`/`canceled` for complete logs.

4. Coordinate many jobs with `enhanced_terminal_list_jobs`:
   - Use it to provide a consolidated view (e.g., to the UI) of `running/finished/canceled`, `runtime_secs`, `preview`, and `command`.

5. Feed results back into the LLM:
   - Provide summarized outputs (or `full_output` selectively) along with domain context to guide subsequent decisions or edits.

### UI Affordances in the Agent Panel

- Job lifecycle visibility:
  - Use `enhanced_terminal_job_status` results to surface badges or status rows (running/finished/canceled), `runtime_secs`, and a short `preview`.
  - When jobs finish, fetch `full_output` on demand to avoid flooding the UI, then render on expansion.

- Parallel execution guidance:
  - Encourage the model (via system prompt or profile description) to use `enhanced_terminal_async` for concurrent tasks and to avoid blocking on single long-running commands.

- Domain-grounded feedback:
  - Display summaries or definitions returned by the domain MCP server alongside job outputs to help users understand how decisions map to business concepts.


Zed uses the [Model Context Protocol](https://modelcontextprotocol.io/) to interact with context servers.

> The Model Context Protocol (MCP) is an open protocol that enables seamless integration between LLM applications and external data sources and tools. Whether you're building an AI-powered IDE, enhancing a chat interface, or creating custom AI workflows, MCP provides a standardized way to connect LLMs with the context they need.

Check out the [Anthropic news post](https://www.anthropic.com/news/model-context-protocol) and the [Zed blog post](https://zed.dev/blog/mcp) for a general intro to MCP.

## Installing MCP Servers

### As Extensions

One of the ways you can use MCP servers in Zed is by exposing them as an extension.
To learn how to create your own, check out the [MCP Server Extensions](../extensions/mcp-extensions.md) page for more details.

Thanks to our awesome community, many MCP servers have already been added as extensions.
You can check which ones are available via any of these routes:

1. [the Zed website](https://zed.dev/extensions?filter=context-servers)
2. in the app, open the Command Palette and run the `zed: extensions` action
3. in the app, go to the Agent Panel's top-right menu and look for the "View Server Extensions" menu item

In any case, here are some of the ones available:

- [Context7](https://zed.dev/extensions/context7-mcp-server)
- [GitHub](https://zed.dev/extensions/github-mcp-server)
- [Puppeteer](https://zed.dev/extensions/puppeteer-mcp-server)
- [Gem](https://zed.dev/extensions/gem)
- [Brave Search](https://zed.dev/extensions/brave-search-mcp-server)
- [Prisma](https://github.com/aqrln/prisma-mcp-zed)
- [Framelink Figma](https://zed.dev/extensions/framelink-figma-mcp-server)
- [Linear](https://zed.dev/extensions/linear-mcp-server)
- [Resend](https://zed.dev/extensions/resend-mcp-server)

### As Custom Servers

Creating an extension is not the only way to use MCP servers in Zed.
You can connect them by adding their commands directly to your `settings.json`, like so:

```json [settings]
{
  "context_servers": {
    "your-mcp-server": {
      "source": "custom",
      "command": "some-command",
      "args": ["arg-1", "arg-2"],
      "env": {}
    }
  }
}
```

Alternatively, you can also add a custom server by accessing the Agent Panel's Settings view (also accessible via the `agent: open settings` action).
From there, you can add it through the modal that appears when you click the "Add Custom Server" button.

## Using MCP Servers

### Configuration Check

Regardless of how you've installed MCP servers, whether as an extension or adding them directly, most servers out there still require some sort of configuration as part of the set up process.

In the case of server extensions, after installing it, Zed will pop up a modal displaying what is required for you to properly set it up.
For example, the GitHub MCP extension requires you to add a [Personal Access Token](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens).

In the case of custom servers, make sure you check the provider documentation to determine what type of command, arguments, and environment variables need to be added to the JSON.

To check if your MCP server is properly configured, go to the Agent Panel's settings view and watch the indicator dot next to its name.
If they're running correctly, the indicator will be green and its tooltip will say "Server is active".
If not, other colors and tooltip messages will indicate what is happening.

### Using it in the Agent Panel

Once installation is complete, you can return to the Agent Panel and start prompting.

Some models are better than others when it comes to picking up tools from MCP servers.
Mentioning your server by name always helps the model to pick it up.

However, if you want to ensure a given MCP server will be used, you can create [a custom profile](./agent-panel.md#custom-profiles) where all built-in tools (or the ones that could cause conflicts with the server's tools) are turned off and only the tools coming from the MCP server are turned on.

As an example, [the Dagger team suggests](https://container-use.com/agent-integrations#zed) doing that with their [Container Use MCP server](https://zed.dev/extensions/mcp-server-container-use):

```json [settings]
"agent": {
  "profiles": {
    "container-use": {
      "name": "Container Use",
      "tools": {
        "fetch": true,
        "thinking": true,
        "copy_path": false,
        "find_path": false,
        "delete_path": false,
        "create_directory": false,
        "list_directory": false,
        "diagnostics": false,
        "read_file": false,
        "open": false,
        "move_path": false,
        "grep": false,
        "edit_file": false,
        "terminal": false
      },
      "enable_all_context_servers": false,
      "context_servers": {
        "container-use": {
          "tools": {
            "environment_create": true,
            "environment_add_service": true,
            "environment_update": true,
            "environment_run_cmd": true,
            "environment_open": true,
            "environment_file_write": true,
            "environment_file_read": true,
            "environment_file_list": true,
            "environment_file_delete": true,
            "environment_checkpoint": true
          }
        }
      }
    }
  }
}
```

### Tool Approval

Zed's Agent Panel includes the `agent.always_allow_tool_actions` setting that, if set to `false`, will require you to give permission for any editing attempt as well as tool calls coming from MCP servers.

You can change this by setting this key to `true` in either your `settings.json` or through the Agent Panel's settings view.
