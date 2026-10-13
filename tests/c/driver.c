// Cross-check driver modelled on pkmn's examples/c/example.c. Reads a serialized Gen 1 battle
// (384 bytes as hex on stdin), plays it to completion with both players choosing uniformly via
// libpkmn's PSRNG seeded from argv[1] (exactly like the example), and prints:
//   <result> <turns> <fnv1a64 of all log bytes> <fnv1a64 of final battle bytes>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <pkmn.h>

static uint64_t fnv(uint64_t h, const uint8_t *p, size_t n) {
   for (size_t i = 0; i < n; i++) { h ^= p[i]; h *= 0x100000001b3ULL; }
   return h;
}

static pkmn_choice choose(pkmn_gen1_battle *battle, pkmn_psrng *random, pkmn_player player,
                          pkmn_choice_kind request, pkmn_choice choices[]) {
   uint8_t n = pkmn_gen1_battle_choices(battle, player, request, choices, PKMN_GEN1_CHOICES_SIZE);
   if (!n) { fprintf(stderr, "no choices\n"); exit(2); }
   return choices[(uint64_t)pkmn_psrng_next(random) * n / 0x100000000];
}

int main(int argc, char **argv) {
   if (argc != 2) { fprintf(stderr, "usage: %s <seed> < battle.hex\n", argv[0]); return 1; }
   uint64_t seed = strtoull(argv[1], NULL, 10);
   pkmn_gen1_battle battle;
   for (size_t i = 0; i < PKMN_GEN1_BATTLE_SIZE; i++) {
      unsigned v;
      if (scanf("%2x", &v) != 1) { fprintf(stderr, "bad input\n"); return 1; }
      battle.bytes[i] = (uint8_t)v;
   }
   pkmn_psrng random;
   pkmn_psrng_init(&random, seed);
   pkmn_choice choices[64];
   uint8_t buf[256];
   pkmn_gen1_log_options log_options = {.buf = buf, .len = sizeof(buf)};
   pkmn_gen1_battle_options options;
   uint64_t h = 0xcbf29ce484222325ULL;
   pkmn_result result;
   pkmn_choice c1 = 0, c2 = 0;
   for (;;) {
      memset(buf, 0, sizeof(buf));
      pkmn_gen1_battle_options_set(&options, &log_options, NULL, NULL);
      result = pkmn_gen1_battle_update(&battle, c1, c2, &options);
      h = fnv(h, buf, sizeof(buf));
      if (pkmn_result_type(result)) break;
      c1 = choose(&battle, &random, PKMN_PLAYER_P1, pkmn_result_p1(result), choices);
      c2 = choose(&battle, &random, PKMN_PLAYER_P2, pkmn_result_p2(result), choices);
   }
   uint16_t turns = battle.bytes[368] | battle.bytes[369] << 8;
   printf("%u %u %016llx %016llx\n", pkmn_result_type(result), turns, (unsigned long long)h,
          (unsigned long long)fnv(0xcbf29ce484222325ULL, battle.bytes, PKMN_GEN1_BATTLE_SIZE));
   return 0;
}
