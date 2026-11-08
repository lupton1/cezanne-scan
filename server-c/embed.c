/*
 * Build helper: turns the web page files into C arrays so cezanne-server is one file.
 *
 *   embed assets.h NAME file [NAME file ...]
 */
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv)
{
    FILE *out;
    int i;

    if (argc < 4 || argc % 2 != 0) {
        fprintf(stderr, "usage: embed out.h NAME file [NAME file ...]\n");
        return 2;
    }
    out = fopen(argv[1], "w");
    if (out == NULL) {
        perror(argv[1]);
        return 1;
    }
    fprintf(out, "/* made by embed.c from crates/cezanne-server/web, do not edit */\n");
    for (i = 2; i + 1 < argc; i += 2) {
        FILE *in = fopen(argv[i + 1], "rb");
        int c;
        unsigned long n = 0;
        if (in == NULL) {
            perror(argv[i + 1]);
            fclose(out);
            remove(argv[1]);
            return 1;
        }
        fprintf(out, "static const unsigned char %s[] = {", argv[i]);
        while ((c = fgetc(in)) != EOF) {
            fprintf(out, "%s%u", n % 24 == 0 ? "\n" : "", (unsigned)c);
            fputc(',', out);
            n++;
        }
        fclose(in);
        fprintf(out, "\n};\n");
    }
    fclose(out);
    return 0;
}
