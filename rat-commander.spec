Name:           rat-commander
Version:        1.9.5
Release:        1%{?dist}
Summary:        A modern terminal file manager inspired by Norton Commander

License:        GPL-2.0-only
URL:            https://github.com/dividebysandwich/rat-commander
Source0:        https://github.com/dividebysandwich/rat-commander/archive/refs/tags/v%{version}.tar.gz

%global debug_package %{nil}

BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  cmake
BuildRequires:  make
BuildRequires:  pkgconfig
BuildRequires:  alsa-lib-devel

%description
Rat Commander is a self-contained terminal file manager inspired by
Norton Commander and Midnight Commander.

It provides a two-panel interface with built-in file viewing and editing,
archive handling, FTP/SFTP/SCP support, Git integration, disk and process
explorers, file comparison, directory synchronization, checksumming,
terminal graphics and other utilities.

The installed executable is named rc.

%prep
%autosetup -n rat-commander-%{version}

%build
# Install Rust and Cargo using rustup.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

export PATH="$PATH:$HOME/.cargo/bin"

rustc --version
cargo --version

# Use the OS compiler/linker for native components.
export CC=gcc
export CXX=g++
export RUSTFLAGS="${RUSTFLAGS} -C linker=gcc"
cargo build --release --locked

%install
install -Dpm0755 target/release/rc \
    %{buildroot}%{_bindir}/rc

%files
%license LICENSE
%doc README.md
%{_bindir}/rc

%changelog
* Tue Sep 22 2026 Rat Commander COPR Maintainer <danie.dejager@gmail.com> - 1.9.5-1
* Tue Sep 22 2026 Rat Commander COPR Maintainer <danie.dejager@gmail.com> - 1.9.4-1
* Wed Sep 15 2026 Rat Commander COPR Maintainer <danie.dejager@gmail.com> - 1.9.3-1
* Tue Sep 15 2026 Rat Commander COPR Maintainer <danie.dejager@gmail.com> - 1.9.1-2
* Mon Sep 14 2026 Rat Commander COPR Maintainer <danie.dejager@gmail.com> - 1.9.1-1
* Mon Sep 14 2026 Rat Commander COPR Maintainer <danie.dejager@gmail.com> - 1.7.6-2
- Build with the current Rust toolchain from rustup on all targets
- Use the system compiler and linker
