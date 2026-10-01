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
first. Use a normal native location such as `~/Library/Application Support/Steve`
for private configuration/data and `~/Applications/Steve.app` for the app.
Managed document folders can block a standalone app's file access or alter
bundle metadata. Build seals the app on temporary native storage and verifies
a fresh sibling copy before replacing its own previous bundle, without clearing quarantine or provenance attributes.
The bundle contains the daemon and the configuration **path**, never a
credential value or copy of the configuration. The bundle gets a local ad-hoc
development signature; it is not a notarized distributable installer.

Launch with the operator-approved configured credential variable inherited in the
environment; never place its value in command arguments or an app launcher file:

```bash
/absolute/output/Steve.app/Contents/MacOS/Steve
```

The daemon validates the configured credential names, including custom names
and Anthropic credentials; the app does not assume `OPENAI_API_KEY` is universal.
The app starts the bundled daemon only after connection refusal establishes
absence. Timeouts, malformed responses and HTTP errors remain inconclusive and
cannot trigger startup. It owns only
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

Double-click the installed app or use `open ~/Applications/Steve.app` to reopen it.
A fresh ordinary launch was verified on the acceptance Mac using its existing
approved environment. Availability of that credential after reboot was not
verified: Finder requires an existing credential-supplying environment or an
already managed daemon; the app itself stores no key. OS keychain storage and login auto-start need separate
authorization and are not installed by this app.

Build runs the executable's `--self-test` checks for client settings, accounting
summary and readiness/presence distinction. Hosted macOS CI builds a fixture
bundle and runs those checks. The real Mac acceptance additionally opens the
window, verifies visible readiness/model/accounting controls, selects Luna and
copies nonsecret settings, exercises Stop/Start, and closes/reopens the window
while observing daemon readiness. Those checks send no paid inference calls.
