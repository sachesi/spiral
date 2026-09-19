%define _debugsource_template %{nil}
%define debug_package %{nil}

%global app_id io.github.sachesi.spiral

Name:           spiral
# The release workflow sets Version to the tag it builds; OBS counts the Release.
Version:        0.17.0
Release:        0
Summary:        File manager for Wayland with a file chooser portal backend

License:        GPL-3.0-or-later
URL:            https://github.com/sachesi/spiral
# Named as the Debian source package names them, which OBS builds from the same files.
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}_%{version}.orig.tar.gz
# The crates the build needs, from the release, so that it runs without a network.
Source1:        %{url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.xz#/%{name}_%{version}.orig-vendor.tar.xz

BuildRequires:  cargo
BuildRequires:  rust >= 1.92
BuildRequires:  gcc
BuildRequires:  blueprint-compiler
BuildRequires:  desktop-file-utils
BuildRequires:  gettext-tools
BuildRequires:  AppStream
BuildRequires:  pkgconfig(gtk4) >= 4.22
BuildRequires:  pkgconfig(libadwaita-1) >= 1.9
BuildRequires:  pkgconfig(gtksourceview-5)
BuildRequires:  pkgconfig(glib-2.0) >= 2.80
BuildRequires:  pkgconfig(libseccomp)
BuildRequires:  pkgconfig(gstreamer-1.0)
# The details panel reads recordings and videos through GStreamer's discoverer.
BuildRequires:  pkgconfig(gstreamer-pbutils-1.0)
BuildRequires:  pkgconfig(gstreamer-audio-1.0)
BuildRequires:  pkgconfig(gstreamer-video-1.0)
# Decoded video reaches the player as DMA-BUFs.
BuildRequires:  pkgconfig(gstreamer-allocators-1.0)
BuildRequires:  pkgconfig(gstreamer-tag-1.0)

Requires:       libgtk-4-1 >= 4.22
Requires:       libadwaita-1-0 >= 1.9
Requires:       hicolor-icon-theme
Requires:       xdg-desktop-portal
# Thumbnailers, the PDF previewer and archive tools only ever run inside bubblewrap;
# without it they are turned off rather than run unconfined.
Requires:       bubblewrap
# The preview plays media through playbin3 drawing into the GTK 4 paintable sink.
Requires:       gstreamer-plugins-base
# openSUSE ships the GTK 4 sink among the Rust plugins.
Requires:       gstreamer-plugins-rs
Recommends:     gstreamer-plugins-good
# What decodes the files people have: libav carries most of the decoders and
# the parsers in bad feed them.
Recommends:     gstreamer-plugins-libav
Recommends:     gstreamer-plugins-bad
# "Remember View per Folder" stores the view and the sort order in metadata::
# attributes, which need the gvfs metadata backend.
Recommends:     gvfs
# Pictures are decoded by glycin in its own sandbox when it is there, looked for at run
# time; without it Spiral decodes them in its own sandbox instead.
Recommends:     libglycin-2-0
Recommends:     glycin-loaders
Suggests:       7zip
Suggests:       xdg-terminal-exec

%description
Spiral is a file manager for Wayland, built with GTK 4 and libadwaita: grid,
list and column views, tabs, a places sidebar with devices and bookmarks, file
operations with progress, conflict handling and undo, archives through the tools
installed on the system, sandboxed thumbnails, a quick preview, a details panel,
and "Open in Terminal".

It provides the org.freedesktop.FileManager1 service and a FileChooser
portal backend, so applications that open and save files through
xdg-desktop-portal get the same dialog.

%prep
%autosetup -n %{name}-%{version} -b 1

%build
export CARGO_HOME="$PWD/.cargo-home"
export RUSTFLAGS="%{?build_rustflags}"
export SPIRAL_LIBEXECDIR="%{_libexecdir}"
export SPIRAL_LOCALEDIR="%{_datadir}/locale"
%if 0%{?_cargo_target_dir:1}
export CARGO_TARGET_DIR="%{_cargo_target_dir}"
%endif
cargo build --release --offline --locked

%install
%if 0%{?_cargo_target_dir:1}
target="%{_cargo_target_dir}/release"
%else
target="target/release"
%endif
install -Dpm 0755 "$target/spiral" %{buildroot}%{_bindir}/spiral
install -Dpm 0755 "$target/xdg-desktop-portal-spiral" %{buildroot}%{_libexecdir}/xdg-desktop-portal-spiral
install -Dpm 0755 "$target/spiral-thumbnailer" %{buildroot}%{_libexecdir}/spiral-thumbnailer

install -d %{buildroot}%{_datadir}/applications %{buildroot}%{_datadir}/metainfo
msgfmt --desktop --template=data/%{app_id}.desktop -d po \
  -o %{buildroot}%{_datadir}/applications/%{app_id}.desktop
msgfmt --xml --template=data/%{app_id}.metainfo.xml -d po \
  -o %{buildroot}%{_datadir}/metainfo/%{app_id}.metainfo.xml
install -Dpm 0644 data/%{app_id}.gschema.xml %{buildroot}%{_datadir}/glib-2.0/schemas/%{app_id}.gschema.xml
install -Dpm 0644 data/icons/hicolor/scalable/apps/%{app_id}.svg \
  %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
install -Dpm 0644 data/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg \
  %{buildroot}%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg
install -Dpm 0644 data/spiral.portal %{buildroot}%{_datadir}/xdg-desktop-portal/portals/spiral.portal
install -Dpm 0644 data/spiral-odf.thumbnailer %{buildroot}%{_datadir}/thumbnailers/spiral-odf.thumbnailer

install -d %{buildroot}%{_datadir}/dbus-1/services
sed 's|@bindir@|%{_bindir}|' data/%{app_id}.FileManager1.service.in \
  > %{buildroot}%{_datadir}/dbus-1/services/%{app_id}.FileManager1.service
sed 's|@libexecdir@|%{_libexecdir}|' data/org.freedesktop.impl.portal.desktop.spiral.service.in \
  > %{buildroot}%{_datadir}/dbus-1/services/org.freedesktop.impl.portal.desktop.spiral.service
sed 's|@libexecdir@|%{_libexecdir}|' data/xdg-desktop-portal-spiral.desktop.in \
  > xdg-desktop-portal-spiral.desktop.in
msgfmt --desktop --template=xdg-desktop-portal-spiral.desktop.in -d po \
  -o %{buildroot}%{_datadir}/applications/xdg-desktop-portal-spiral.desktop

for lang in $(cat po/LINGUAS); do
  install -d %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES
  msgfmt -o %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES/%{name}.mo po/$lang.po
done
%find_lang %{name}

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
desktop-file-validate %{buildroot}%{_datadir}/applications/xdg-desktop-portal-spiral.desktop
appstreamcli validate --no-net %{buildroot}%{_datadir}/metainfo/%{app_id}.metainfo.xml
test -x %{buildroot}%{_bindir}/spiral

%files -f %{name}.lang
%license LICENSE
%doc README.md docs
%{_bindir}/spiral
%{_libexecdir}/xdg-desktop-portal-spiral
%{_libexecdir}/spiral-thumbnailer
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/applications/xdg-desktop-portal-spiral.desktop
%{_datadir}/metainfo/%{app_id}.metainfo.xml
%{_datadir}/glib-2.0/schemas/%{app_id}.gschema.xml
%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg
# openSUSE wants every directory owned; nothing required here owns these.
%dir %{_datadir}/xdg-desktop-portal
%dir %{_datadir}/xdg-desktop-portal/portals
%{_datadir}/xdg-desktop-portal/portals/spiral.portal
%dir %{_datadir}/thumbnailers
%{_datadir}/thumbnailers/spiral-odf.thumbnailer
%{_datadir}/dbus-1/services/%{app_id}.FileManager1.service
%{_datadir}/dbus-1/services/org.freedesktop.impl.portal.desktop.spiral.service

%changelog
