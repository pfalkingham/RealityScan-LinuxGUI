# Install realityscan-gtk for the current user (PREFIX=~/.local) or system-wide (PREFIX=/usr/local).
PREFIX ?= $(HOME)/.local
APP_ID = org.realityscan_gtk.RealityScanGtk
BIN = target/release/realityscan-gtk

.PHONY: build dxcore install uninstall

build:
	cargo build --release -p realityscan-gtk

# The dxcore.dll that gives RealityScan's AI masking the GPU (needs winegcc+clang). Built by
# `install` whenever the toolchain is present, so a plain clone + `make install` gives the GPU
# path; skipped with a note when it isn't (the app still works, masking on the CPU).
dxcore:
	@if command -v winegcc >/dev/null 2>&1; then \
		$(MAKE) -C tools/dxcore; \
	else \
		echo "note: winegcc not found, not building dxcore.dll; AI masking will stay on the CPU (Arch: pacman -S wine clang)"; \
	fi

install: build dxcore
	install -Dm755 $(BIN) $(DESTDIR)$(PREFIX)/bin/realityscan-gtk
	install -Dm644 packaging/$(APP_ID).desktop $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 packaging/$(APP_ID).svg $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 packaging/$(APP_ID).metainfo.xml $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
	@if [ -f tools/dxcore/dxcore.dll ]; then \
		install -Dm644 tools/dxcore/dxcore.dll $(DESTDIR)$(PREFIX)/share/realityscan-gtk/dxcore.dll; \
	else echo "note: tools/dxcore/dxcore.dll not built (run 'make dxcore' with winegcc installed); AI masking will stay on the CPU"; fi
	-update-desktop-database $(DESTDIR)$(PREFIX)/share/applications 2>/dev/null
	-gtk4-update-icon-cache -q -t $(DESTDIR)$(PREFIX)/share/icons/hicolor 2>/dev/null

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/realityscan-gtk
	rm -f $(DESTDIR)$(PREFIX)/share/applications/$(APP_ID).desktop
	rm -f $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(APP_ID).svg
	rm -f $(DESTDIR)$(PREFIX)/share/metainfo/$(APP_ID).metainfo.xml
	rm -f $(DESTDIR)$(PREFIX)/share/realityscan-gtk/dxcore.dll
