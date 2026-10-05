# Build the optional anytopdf-printer helper (Linux and macOS).
#
# Requires PAPPL 1.3+ and libcups development files, found through pkg-config:
#   Debian/Ubuntu: apt install libpappl-dev pkg-config
#   macOS:         brew install pappl pkg-config

PKG_CONFIG ?= pkg-config
CC ?= cc
CFLAGS ?= -O2 -g
WARNINGS ?= -Wall -Wextra -Wno-unused-parameter
PREFIX ?= /usr/local
VERSION ?= $(shell sed -n 's/^version = "\(.*\)"/\1/p' ../../Cargo.toml | head -n 1)

PAPPL_CFLAGS = $(shell $(PKG_CONFIG) --cflags pappl)
PAPPL_LIBS = $(shell $(PKG_CONFIG) --libs pappl)

.PHONY: all check-deps clean install

all: anytopdf-printer

check-deps:
	@$(PKG_CONFIG) --exists 'pappl >= 1.3' || { \
	  echo 'anytopdf-printer needs PAPPL 1.3 or later (libpappl-dev); see README.md' >&2; exit 1; }

anytopdf-printer: anytopdf-printer.c | check-deps
	$(CC) $(CFLAGS) $(WARNINGS) $(PAPPL_CFLAGS) -DANYTOPDF_PRINTER_VERSION='"$(VERSION)"' -o $@ $< $(PAPPL_LIBS) $(LDFLAGS)

install: anytopdf-printer
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m 755 anytopdf-printer $(DESTDIR)$(PREFIX)/bin/anytopdf-printer

clean:
	rm -f anytopdf-printer
