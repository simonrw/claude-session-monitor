module: {
  name = "claude-session-monitor";

  nodes.machine = {
    imports = [ module ];

    services.claude-session-monitor.enable = true;
  };

  testScript =
    { nodes, ... }:
    let
      url = "http://127.0.0.1:${toString nodes.machine.services.claude-session-monitor.port}";
    in
    ''
      import json
      import re
      import shlex


      def wait_for_health():
          """Waits until the service answers its health endpoint, then returns the answer."""
          machine.wait_for_unit("claude-session-monitor.service")

          return json.loads(machine.wait_until_succeeds("curl --fail --silent ${url}/api/health"))


      def event_stream_text():
          # The event stream never ends, so curl is cut off once the first event has arrived.
          return machine.succeed("curl --silent --max-time 2 ${url}/api/events || true")


      with subtest("answers HTTP"):
          health = wait_for_health()

          assert health["status"] == "ok", health

      with subtest("serves the frontend"):
          page = machine.succeed("curl --fail --silent ${url}/")
          script = re.search(r'<script[^>]+src="([^"]+)"', page)

          assert script, f"no script in the page:\n{page}"

          machine.succeed(f"curl --fail --silent --output /dev/null ${url}{script.group(1)}")

      with subtest("keeps its database across a restart"):
          session = {
              "session_id": "survives-a-restart",
              "cwd": "/tmp",
              "status": {"type": "idle"},
              "hook_event_name": "Stop",
          }
          machine.succeed(
              f"curl --fail --silent --json {shlex.quote(json.dumps(session))} ${url}/api/sessions"
          )

          assert "survives-a-restart" in event_stream_text()

          machine.systemctl("restart claude-session-monitor.service")
          wait_for_health()

          assert "survives-a-restart" in event_stream_text(), "the session was lost in the restart"
    '';
}
