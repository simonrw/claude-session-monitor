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


      def start():
          machine.wait_for_unit("claude-session-monitor.service")
          machine.wait_until_succeeds("curl --fail --silent ${url}/api/health")


      def sessions():
          # The event stream never ends, so curl is cut off once the first event has arrived.
          return machine.succeed("curl --silent --max-time 2 ${url}/api/events || true")


      start()

      with subtest("answers HTTP"):
          health = json.loads(machine.succeed("curl --fail --silent ${url}/api/health"))

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
          assert "survives-a-restart" in sessions()

          machine.systemctl("restart claude-session-monitor.service")
          start()

          assert "survives-a-restart" in sessions(), "the session was lost in the restart"
    '';
}
