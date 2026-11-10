# Cursor compatibility

The end to end desktop check used Cursor **3.27.22** on macOS. An Agent turn in a previously failing conversation reached a configured provider, received a streamed text response, and completed. Other client versions and platforms may require additional testing.

## Unknown model ID

Cursor can select a managed local runtime that sends `RunInference` messages. The proxy handles Cursor's `AgentService` transport. During bootstrap it disables the `agent_host_local_loop` gate so the connected client uses that transport. If the client still holds an older bootstrap configuration, keep the proxy running and run **Developer: Reload Window** in Cursor before retrying.

If the error persists, check that the service is running, the CA is trusted, and `doctor` reports that the proxy settings are applied. Verify the selected model is still present in the current config and try a new Agent chat.

## Subagents

Some Cursor versions send a subagent kickoff with an empty user message ID or only one parent header. This project derives a stable user message ID and permits a partial header pair without linking the subagent to a parent run. See the original [upstream report](https://github.com/leookun/cursor-byok/pull/515).

The project has automated checks for protocol handling and tool rounds. Provider smoke tests exercise the configured API. Desktop checks also depend on the installed Cursor build.
