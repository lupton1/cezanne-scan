# cezanne-server in C for Mac OS X 10.6 and later. See README.md.
#
#   make          on a current Mac: a 64 bit Intel program for 10.6 and later
#   make native   on the 10.6 Mac itself with Xcode 3.2: 32 and 64 bit Intel
#   make host     a test build for this computer (any Mac or Linux), without the 10.6 settings
#   make check    builds the test build and compares it with the Rust server (compare.py)

SRC = server.c scanner.c scsi.c image.c json.c util.c
HDR = cz.h plat.h assets.h
WEB = ../crates/cezanne-server/web
FRAMEWORKS = -framework IOKit -framework CoreFoundation
# no fused multiply add, so the floating point results match the Rust build exactly
CFLAGS_COMMON = -std=gnu99 -O2 -Wall -Wextra -Wno-unused-parameter -ffp-contract=off

all: cezanne-server

assets.h: embed.c $(WEB)/index.html $(WEB)/app.js $(WEB)/style.css ../packaging/icons/128x128.png
	cc -o embed embed.c
	./embed assets.h ASSET_INDEX_HTML $(WEB)/index.html ASSET_APP_JS $(WEB)/app.js \
		ASSET_STYLE_CSS $(WEB)/style.css ASSET_ICON_PNG ../packaging/icons/128x128.png
	rm -f embed

# Current Xcode can still target 10.6 for x86_64, but only its classic linker writes the
# LC_UNIXTHREAD entry point that 10.6 needs (the new one always writes LC_MAIN, 10.8+).
cezanne-server: $(SRC) $(HDR)
	clang $(CFLAGS_COMMON) -arch x86_64 -mmacosx-version-min=10.6 -Wl,-ld_classic \
		-o $@ $(SRC) $(FRAMEWORKS) 2>&1 | grep -v 'ld_classic is deprecated' || true
	@test -x $@

native: $(SRC) $(HDR)
	gcc -std=gnu99 -O2 -Wall -arch i386 -arch x86_64 -mmacosx-version-min=10.6 \
		-o cezanne-server $(SRC) $(FRAMEWORKS)

host: $(SRC) $(HDR)
	cc $(CFLAGS_COMMON) -o cezanne-server-host $(SRC) $(shell [ `uname` = Darwin ] && echo "$(FRAMEWORKS)") -lpthread -lm

check: host
	python3 compare.py --c ./cezanne-server-host --rust ../target/release/cezanne-server --bridge ../target/release/cezanne-bridge

clean:
	rm -f cezanne-server cezanne-server-host embed assets.h

.PHONY: all native host check clean
