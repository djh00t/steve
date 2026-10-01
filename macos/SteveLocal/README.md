# Steve local app

This small native macOS controller opens a window for the single-user proxy.
It shows actual daemon readiness (including accounting admission), the configured
model catalogue, accounting incident/queue status, and copyable client settings.
It adds no web dashboard, provider calls, service installation or key storage.
The broader M8 menu-bar, installer and account-management platform remains deferred.

Build a private app from an already verified daemon and prepared configuration:

```bash
bash macos/SteveLocal/build.sh /absolute/verified/steve /absolute/private/native.toml /absolute/output/Steve.app
```

The configuration must use inference `127.0.0.1:11435` and management
`127.0.0.1:8790`. Provision its accounting root using the daemon instructions
first. The bundle contains the daemon and the configuration **path**, never a
credential value or copy of the configuration. This controller is an unsigned
local development app; it is not a notarized distributable installer.

Launch with the operator-approved existing `OPENAI_API_KEY` inherited in the
environment; never place its value in command arguments or an app launcher file:

```bash
/absolute/output/Steve.app/Contents/MacOS/Steve
```

The app starts the bundled daemon if no Steve instance is present. It owns only
the daemon it starts. An existing starting, draining or accounting-blocked daemon
is displayed without launching a competitor. The app polls only local status,
readiness and models; it makes no inference or provider-health requests.

Use the model picker and **Copy client settings** to configure an OpenAI-compatible
client. `local` is a nonsecret client-key placeholder; the daemon substitutes its
approved upstream credential. **Show configuration** reveals the private setup
file in Finder. Route/price changes require editing that file and restarting.
Requests you initiate through a client can incur provider charges.

**Stop Steve** sends SIGTERM to the app-owned daemon; **Start Steve** starts it
again. Closing the window keeps the app and proxy alive. Reopening the app brings
back the window. **Quit Steve** waits for its owned daemon to drain gracefully.
It does not stop an externally managed daemon. Logs append to the private
configuration directory's `app-daemon.log`.

After quitting, a fresh Finder launch cannot inherit a terminal's API key.
Launch again from your approved secret-supplying environment, or reconnect to an
already managed daemon. OS keychain storage and login auto-start need separate
authorization and are not installed by this app.

Build runs the executable's `--self-test` checks for client settings, accounting
summary and readiness/presence distinction. Hosted macOS CI builds a fixture
bundle and runs those checks. The real Mac acceptance additionally opens the
window, verifies visible readiness/model/accounting controls, selects Luna and
copies nonsecret settings, exercises Stop/Start, and closes/reopens the window
while observing daemon readiness. Those checks send no paid inference calls.
