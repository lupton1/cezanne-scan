# cezanne-bridge in C for Mac OS X 10.6 and later. See README.md.
#
#   make          on a current Mac: a 64 bit Intel program for 10.6 and later
#   make native   on the 10.6 Mac itself with Xcode 3.2: 32 and 64 bit Intel
#   make relay    test build that can forward to another bridge (never ship it)
#   make check    builds the test build and runs test.sh against the simulator

SRC = cezanne-bridge.c
FRAMEWORKS = -framework IOKit -framework CoreFoundation
WARN = -Wall -Wextra -Wno-unused-parameter

all: cezanne-bridge

# Current Xcode can still target 10.6 for x86_64, but only its classic linker writes the
# LC_UNIXTHREAD entry point that 10.6 needs (the new one always writes LC_MAIN, 10.8+).
cezanne-bridge: $(SRC)
	clang -std=gnu99 -O2 $(WARN) -arch x86_64 -mmacosx-version-min=10.6 -Wl,-ld_classic \
		-o $@ $(SRC) $(FRAMEWORKS) 2>&1 | grep -v 'ld_classic is deprecated' || true
	@test -x $@

native: $(SRC)
	gcc -std=gnu99 -O2 -Wall -arch i386 -arch x86_64 -mmacosx-version-min=10.6 \
		-o cezanne-bridge $(SRC) $(FRAMEWORKS)

relay: $(SRC)
	clang -std=gnu99 -O2 $(WARN) -DCZB_RELAY -o cezanne-bridge-relay $(SRC) $(FRAMEWORKS)

check: relay
	bash test.sh

clean:
	rm -f cezanne-bridge cezanne-bridge-relay

.PHONY: all native relay check clean
