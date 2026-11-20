@echo off
rem Launch the MCP server from a private copy of the release build, so the
rem running server never locks target\release\qucs-s-rs.exe and `cargo build
rem --release` can always replace it. Use it as the MCP command:
rem   "command": "cmd", "args": ["/c", "<repo>\\tools\\qucs-s-rs-mcp.cmd"]
rem Extra arguments are passed on after "mcp".
setlocal
set "SRC=%~dp0..\target\release\qucs-s-rs.exe"
set "DIR=%LOCALAPPDATA%\qucs-s-rs\mcp-bin"
if not exist "%DIR%" mkdir "%DIR%" >nul 2>&1
rem drop copies no longer in use (running ones are locked and stay)
del /q "%DIR%\qucs-s-rs-*.exe" >nul 2>&1
set "DST=%DIR%\qucs-s-rs-%RANDOM%%RANDOM%.exe"
copy /y "%SRC%" "%DST%" >nul 2>&1 || set "DST=%SRC%"
"%DST%" mcp %*
