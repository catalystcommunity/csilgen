import 'dart:io';

import 'types.gen.dart';

// The Dart VM's DateTime keeps microseconds.
const digits = '6';

List<int> unhex(String s) => [
  for (var i = 0; i < s.length; i += 2) int.parse(s.substring(i, i + 2), radix: 16),
];

String hex(List<int> b) => b.map((x) => x.toRadixString(16).padLeft(2, '0')).join();

void main(List<String> args) {
  final failures = <String>[];
  var checked = 0;
  for (final line in File(args[0]).readAsLinesSync()) {
    if (line.isEmpty || line.startsWith('#')) continue;
    final [name, expect, rowDigits, input, canonical] = line.split('\t');
    if (rowDigits != '*' && rowDigits != digits) continue;
    checked++;
    Moment? moment;
    Object? error;
    try {
      moment = Moment.fromCbor(unhex(input));
    } catch (e) {
      error = e;
    }
    if (expect == 'ok' && moment == null) {
      failures.add('$name: decode failed: $error');
    } else if (expect == 'ok') {
      final got = hex(moment!.toCbor());
      if (got != canonical) failures.add('$name: encoded $got, want $canonical');
    } else if (moment != null) {
      failures.add('$name: decode accepted ${moment.at}');
    }
  }
  failures.forEach(print);
  if (failures.isNotEmpty) exit(1);
  print('dart epoch vectors: $checked ok');
}
