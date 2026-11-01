@echo off
rem Start Synoptic from a plain cmd/Explorer: puts the MSYS2 UCRT64 runtime on PATH and logs to synoptic-windows.log in the repo.
set PATH=C:\msys64\ucrt64\bin;%PATH%
set RUST_LOG=synoptic=debug,ss_stream=info
set GST_DEBUG=2
cd /d %~dp0..\..
target\release\synoptic.exe > synoptic-windows.log 2>&1
