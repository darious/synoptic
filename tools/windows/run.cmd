@echo off
rem Start Synoptic from a plain cmd/Explorer: puts the MSYS2 UCRT64 runtime on PATH and logs to synoptic-windows.log in the repo.
set PATH=C:\msys64\ucrt64\bin;%PATH%
set RUST_LOG=synoptic=debug,ss_stream=info
set GST_DEBUG=2
rem first audio frames of every stream, as hex, for AAC framing analysis
set SYNOPTIC_AUDIO_DUMP=%USERPROFILE%\audio-dump.txt
rem first access units of every video stream, Annex-B, to replay offline
set SYNOPTIC_VIDEO_DUMP=%USERPROFILE%\video-dump
cd /d %~dp0..\..
target\release\synoptic.exe > synoptic-windows.log 2>&1
