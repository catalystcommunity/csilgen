import 'dart:io';

import 'types.gen.dart';

// The Dart VM's DateTime keeps microseconds.
const digits = 6;

List<int> unhex(String s) => [
  for (var i = 0; i < s.length; i += 2) int.parse(s.substring(i, i + 2), radix: 16),
];

String hex(List<int> b) => b.map((x) => x.toRadixString(16).padLeft(2, '0')).join();

void main(List<String> args) {
  final failures = <String>[];
  for (final line in File(args[0]).readAsLinesSync()) {
    if (line.isEmpty || line.startsWith('#')) continue;
    final [name, expect, precision, input, canonical] = line.split('\t');
    Stamp? stamp;
    Object? error;
    try {
      stamp = Stamp.fromCbor(unhex(input));
    } catch (e) {
      error = e;
    }
    final wantOk = expect == 'ok' && int.parse(precision) <= digits;
    if (wantOk && stamp == null) {
      failures.add('$name: decode failed: $error');
    } else if (wantOk) {
      final got = hex(stamp!.toCbor());
      if (got != canonical) failures.add('$name: encoded $got');
    } else if (stamp != null) {
      failures.add('$name: decode accepted ${stamp.at}');
    }
  }
  failures.forEach(print);
  if (failures.isNotEmpty) exit(1);
  print('dart: all timestamp vectors pass');
}
