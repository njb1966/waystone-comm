# Waystone Comm Production Smoke Checklist

## Directory

1. Open Waystone Comm normally.
2. Create a new Telnet entry for GameSrv.
3. Set emulation to `ansi-bbs`.
4. Assign it to a group with `G`.
5. Edit it and confirm group, emulation, host, and port persist.
6. Close and reopen Waystone Comm, then confirm the entry still appears under that group.

## Telnet ANSI-BBS

1. Connect to GameSrv through the saved entry.
2. Confirm the session behaves as an 80-column ANSI session.
3. Launch LoRD.
4. Confirm title and menu ANSI screens render without horizontal breakage or scrolling over themselves.
5. Move through several menus and return to the main menu.

## SSH ANSI-BBS

1. Connect to a known SSH BBS using `ansi-bbs`.
2. Confirm ANSI art, box drawing, cursor movement, and menu prompts render correctly.
3. Resize the local terminal wider than 80 columns and confirm ANSI-BBS stays stable.

## Transfers

1. Upload a small text file with Zmodem.
2. Confirm the remote system reports upload completion.
3. Download a small file with Zmodem.
4. Confirm the local file exists and byte size/content match.
5. Cancel a transfer and confirm Waystone Comm recovers cleanly.

## Credentials And Scripts

1. Create a password credential with username and password.
2. Attach it to a test directory entry.
3. Create an entry script:

   ```rhai
   fn on_connect(s) {
       s.log("user=" + s.credential("username"));
       s.log("password-present=" + if s.credential("password") == "" { "no" } else { "yes" });
   }
   ```

4. Connect and confirm the script log shows the username and `password-present=yes`.
5. Test a script with `s.disconnect()`:

   ```rhai
   fn on_connect(s) {
       s.log("disconnecting");
       s.disconnect();
   }
   ```

6. Confirm the session disconnects cleanly and does not hang.

## Logs And History

1. Connect to at least one entry.
2. Disconnect normally.
3. Open the log viewer.
4. Confirm session text is present and readable.
5. Confirm history and last-connected update in the directory.

## Key Mapping

1. Confirm F-key bar actions still work.
2. Run a named script from its key binding.
3. Confirm malformed/custom key mappings do not crash the app.

## Pass Criteria

- No panic.
- Terminal restores cleanly on exit.
- No stuck transfer state.
- No broken ANSI screen state.
- Directory changes persist.
- Scripts do not expose secrets in normal logs unless explicitly logged by the script.
