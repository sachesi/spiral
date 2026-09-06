# Spiral build and install tasks.
#
# `build` needs cargo and blueprint-compiler; `install` only copies what is already in
# target/release, so the two can run on different machines sharing this directory.
#
#   just build
#   sudo just install              (prefix /usr/local)
#   just prefix=$HOME/.local install
#   just setup-portal              (once, per user)

set shell := ["bash", "-euo", "pipefail", "-c"]

app_id := "io.github.sachesi.spiral"
prefix := env("PREFIX", "/usr/local")
destdir := env("DESTDIR", "")
bindir := destdir + prefix + "/bin"
libexecdir := destdir + prefix + "/libexec"
datadir := destdir + prefix + "/share"
release := "target/release"
schema_dir := "target/schemas"
pot_dir := "target/pot"
version := `sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml`

default:
    @just --list

# Release build of all three binaries.
build:
    cargo build --release

# Debug build.
build-debug:
    cargo build

# Compile the GSettings schema into target/schemas for running uninstalled.
schemas:
    mkdir -p {{schema_dir}}
    cp data/{{app_id}}.gschema.xml {{schema_dir}}/
    glib-compile-schemas {{schema_dir}}

# Run the debug build uninstalled: just run [PATH]
run *args: build-debug schemas
    GSETTINGS_SCHEMA_DIR={{schema_dir}} target/debug/spiral {{args}}

# Run the portal backend uninstalled (needs a session bus and xdg-desktop-portal).
run-portal: build-debug schemas
    GSETTINGS_SCHEMA_DIR={{schema_dir}} target/debug/xdg-desktop-portal-spiral

# Lints: rustfmt, clippy, blueprint, desktop file and metainfo validation.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    blueprint-compiler batch-compile /tmp/spiral-blp-check data/ui data/ui/*.blp >/dev/null
    desktop-file-validate data/{{app_id}}.desktop
    sed 's|@libexecdir@|/usr/libexec|' data/xdg-desktop-portal-spiral.desktop.in > /tmp/spiral-blp-check/portal.desktop && desktop-file-validate /tmp/spiral-blp-check/portal.desktop
    appstreamcli validate --no-net data/{{app_id}}.metainfo.xml
    for lang in $(cat po/LINGUAS); do msgfmt -c -o /dev/null po/$lang.po; done

# Regenerate po/spiral.pot from the Rust sources, the Blueprint files, the desktop entries,
# the metainfo and the schema.
pot:
    mkdir -p {{pot_dir}}/ui
    blueprint-compiler batch-compile {{pot_dir}}/ui data/ui data/ui/*.blp >/dev/null
    sed 's|@libexecdir@|/usr/libexec|' data/xdg-desktop-portal-spiral.desktop.in > {{pot_dir}}/xdg-desktop-portal-spiral.desktop
    # xgettext has no Rust mode; the C lexer copes once lifetimes ('a, 'static) are stripped.
    rm -rf {{pot_dir}}/src && cp -r src {{pot_dir}}/src
    find {{pot_dir}}/src -name '*.rs' -exec sed -i -E "s/'([A-Za-z_][A-Za-z0-9_]*)([^'A-Za-z0-9_]|$)/\1\2/g" {} +
    xgettext --from-code=UTF-8 --package-name=spiral --package-version={{version}} \
        --msgid-bugs-address=https://github.com/sachesi/spiral/issues \
        --language=C --keyword= --keyword=gettext --keyword=ngettext:1,2 \
        --flag=gettext:1:no-c-format --flag=ngettext:1:no-c-format --flag=ngettext:2:no-c-format \
        --add-comments=Translators --sort-by-file --directory={{pot_dir}} -o po/spiral.pot $(cd {{pot_dir}} && find src -name '*.rs' | sort)
    xgettext -j --from-code=UTF-8 --package-name=spiral --package-version={{version}} --msgid-bugs-address=https://github.com/sachesi/spiral/issues --add-comments=Translators --sort-by-file --directory={{pot_dir}} -o po/spiral.pot $(cd {{pot_dir}} && ls ui/*.ui)
    xgettext -j --from-code=UTF-8 --package-name=spiral --package-version={{version}} --msgid-bugs-address=https://github.com/sachesi/spiral/issues --language=Desktop --sort-by-file -o po/spiral.pot data/{{app_id}}.desktop {{pot_dir}}/xdg-desktop-portal-spiral.desktop
    xgettext -j --from-code=UTF-8 --package-name=spiral --package-version={{version}} --msgid-bugs-address=https://github.com/sachesi/spiral/issues --sort-by-file -o po/spiral.pot data/{{app_id}}.metainfo.xml data/{{app_id}}.gschema.xml

# Merge the current template into every po/<lang>.po.
po: pot
    for lang in $(cat po/LINGUAS); do msgmerge --update --backup=none --quiet po/$lang.po po/spiral.pot; done
    for lang in $(cat po/LINGUAS); do msgfmt --statistics -o /dev/null po/$lang.po; done

test:
    cargo test

# Install the release build. Does not build: run `just build` first.
install:
    @test -x {{release}}/spiral -a -x {{release}}/xdg-desktop-portal-spiral -a -x {{release}}/spiral-thumbnailer || { echo "error: {{release}}/spiral missing; run 'just build' first" >&2; exit 1; }
    install -Dm755 {{release}}/spiral {{bindir}}/spiral
    install -Dm755 {{release}}/xdg-desktop-portal-spiral {{libexecdir}}/xdg-desktop-portal-spiral
    install -Dm755 {{release}}/spiral-thumbnailer {{libexecdir}}/spiral-thumbnailer
    mkdir -p {{datadir}}/applications {{datadir}}/metainfo
    msgfmt --desktop --template=data/{{app_id}}.desktop -d po -o {{datadir}}/applications/{{app_id}}.desktop
    msgfmt --xml --template=data/{{app_id}}.metainfo.xml -d po -o {{datadir}}/metainfo/{{app_id}}.metainfo.xml
    install -Dm644 data/{{app_id}}.gschema.xml {{datadir}}/glib-2.0/schemas/{{app_id}}.gschema.xml
    install -Dm644 data/icons/hicolor/scalable/apps/{{app_id}}.svg {{datadir}}/icons/hicolor/scalable/apps/{{app_id}}.svg
    install -Dm644 data/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg {{datadir}}/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg
    install -Dm644 data/spiral.portal {{datadir}}/xdg-desktop-portal/portals/spiral.portal
    install -Dm644 data/spiral-odf.thumbnailer {{datadir}}/thumbnailers/spiral-odf.thumbnailer
    mkdir -p {{datadir}}/dbus-1/services
    sed 's|@bindir@|{{prefix}}/bin|' data/org.freedesktop.FileManager1.service.in > {{datadir}}/dbus-1/services/org.freedesktop.FileManager1.service
    sed 's|@libexecdir@|{{prefix}}/libexec|' data/org.freedesktop.impl.portal.desktop.spiral.service.in > {{datadir}}/dbus-1/services/org.freedesktop.impl.portal.desktop.spiral.service
    sed 's|@libexecdir@|{{prefix}}/libexec|' data/xdg-desktop-portal-spiral.desktop.in > {{datadir}}/applications/xdg-desktop-portal-spiral.desktop.in
    msgfmt --desktop --template={{datadir}}/applications/xdg-desktop-portal-spiral.desktop.in -d po -o {{datadir}}/applications/xdg-desktop-portal-spiral.desktop
    rm {{datadir}}/applications/xdg-desktop-portal-spiral.desktop.in
    for lang in $(cat po/LINGUAS); do install -d {{datadir}}/locale/$lang/LC_MESSAGES; msgfmt -o {{datadir}}/locale/$lang/LC_MESSAGES/spiral.mo po/$lang.po; done
    glib-compile-schemas {{datadir}}/glib-2.0/schemas
    update-desktop-database -q {{datadir}}/applications || true
    gtk4-update-icon-cache -qtf {{datadir}}/icons/hicolor || gtk-update-icon-cache -qtf {{datadir}}/icons/hicolor || true
    @echo "installed to {{prefix}}"

uninstall:
    rm -f {{bindir}}/spiral {{libexecdir}}/xdg-desktop-portal-spiral {{libexecdir}}/spiral-thumbnailer
    rm -f {{datadir}}/applications/{{app_id}}.desktop {{datadir}}/applications/xdg-desktop-portal-spiral.desktop {{datadir}}/metainfo/{{app_id}}.metainfo.xml
    rm -f {{datadir}}/glib-2.0/schemas/{{app_id}}.gschema.xml
    rm -f {{datadir}}/icons/hicolor/scalable/apps/{{app_id}}.svg {{datadir}}/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg
    rm -f {{datadir}}/xdg-desktop-portal/portals/spiral.portal
    rm -f {{datadir}}/thumbnailers/spiral-odf.thumbnailer
    rm -f {{datadir}}/dbus-1/services/org.freedesktop.FileManager1.service {{datadir}}/dbus-1/services/org.freedesktop.impl.portal.desktop.spiral.service
    for lang in $(cat po/LINGUAS); do rm -f {{datadir}}/locale/$lang/LC_MESSAGES/spiral.mo; done
    glib-compile-schemas {{datadir}}/glib-2.0/schemas || true
    update-desktop-database -q {{datadir}}/applications || true

# Make Spiral the default folder handler for the current user.
set-default:
    xdg-mime default {{app_id}}.desktop inode/directory
    @echo "inode/directory -> $(xdg-mime query default inode/directory)"

# Point xdg-desktop-portal's FileChooser at xdg-desktop-portal-spiral for the current user, then restart it.
setup-portal:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="${XDG_CONFIG_HOME:-$HOME/.config}/xdg-desktop-portal"
    mkdir -p "$dir"
    # A <desktop>-portals.conf outranks portals.conf, so edit that one when present.
    conf="$dir/portals.conf"
    IFS=: read -ra desktops <<< "${XDG_CURRENT_DESKTOP:-}"
    for d in "${desktops[@]}"; do
        f="$dir/${d,,}-portals.conf"
        if [ -f "$f" ]; then conf="$f"; break; fi
    done
    touch "$conf"
    grep -q '^\[preferred\]' "$conf" || printf '[preferred]\n' >> "$conf"
    if grep -q '^org.freedesktop.impl.portal.FileChooser=' "$conf"; then
        sed -i 's|^org.freedesktop.impl.portal.FileChooser=.*|org.freedesktop.impl.portal.FileChooser=spiral|' "$conf"
    else
        sed -i '/^\[preferred\]/a org.freedesktop.impl.portal.FileChooser=spiral' "$conf"
    fi
    systemctl --user restart xdg-desktop-portal.service
    echo "FileChooser portal -> spiral ($conf)"

# Remove the portal preference again, from whichever file setup-portal wrote it to.
unset-portal:
    sed -i '/^org.freedesktop.impl.portal.FileChooser=spiral$/d' "${XDG_CONFIG_HOME:-$HOME/.config}/xdg-desktop-portal/"*portals.conf
    systemctl --user restart xdg-desktop-portal.service
