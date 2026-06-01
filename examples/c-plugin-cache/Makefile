# Example Cache Plugin Makefile
# Build with: make

CC ?= gcc
CFLAGS ?= -Wall -Wextra -fPIC -O2
INCLUDES = -I../../crates/factor-plugins/include

TARGET = libcache.so
SRC = src/cache.c

all: $(TARGET)

$(TARGET): $(SRC)
	$(CC) $(CFLAGS) $(INCLUDES) -shared -o $@ $<

clean:
	rm -f $(TARGET)

install: $(TARGET)
	cp $(TARGET) ./

.PHONY: all clean install
