# dxcore.dll replacement for RealityScan's AI masking under Wine/Proton (see README.md).
# Needs winegcc with a clang that can target x86_64-windows (Arch: wine + clang packages).
WINE_INCLUDE ?= /usr/include/wine

dxcore.dll: dxcore.c dxcore.spec wine/debug.h
	winegcc -b x86_64-windows -shared -Wall -O2 -I. -o $@ dxcore.c dxcore.spec

# Wine's debug macros live outside the Windows header tree; copy them in rather than adding
# /usr/include (that would drag glibc headers into a Windows build).
wine/debug.h:
	mkdir -p wine && cp $(WINE_INCLUDE)/debug.h $(WINE_INCLUDE)/exception.h wine/

clean:
	rm -rf dxcore.dll wine

.PHONY: clean
