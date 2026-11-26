bin/<platform>/ holds the executables bundled into the VSIX:
  win32-x64/fascon.exe, win32-x64/fascon-core.exe
  linux-x64/fascon, linux-x64/fascon-core
  linux-arm64/fascon, linux-arm64/fascon-core
  darwin-universal/fascon, darwin-universal/fascon-core
fascon = the 1.0 build of this repository (cargo build --release for each target);
fascon-core = the 0.1.0 executable of the same platform.
packaging/make_release.py fills these folders and runs vsce.
