# Install realityscan-gtk for the current user (PREFIX=~/.local) or system-wide (PREFIX=/usr/local).
PREFIX ?= $(HOME)/.local
APP_ID = org.realityscan_gtk.RealityScanGtk
BIN = target/release/realityscan-gtk

.PHONY: build dxcore install uninstall

build:
	cargo build --release -p realityscan-gtk

# Optional: the dxcore.dll that gives RealityScan's AI masking the GPU (needs winegcc+clang).
dxcore:
	$(MAKE) -C tools/dxcore

install: build
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
