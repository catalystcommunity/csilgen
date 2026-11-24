import 'dart:io';
import 'dart:typed_data';

import 'types.gen.dart';

List<int> unhex(String s) => [
  for (var i = 0; i < s.length; i += 2)
    int.parse(s.substring(i, i + 2), radix: 16),
];

String hex(List<int> b) =>
    b.map((x) => x.toRadixString(16).padLeft(2, '0')).join();

Uint8List roundTrip(String kind, List<int> input) => switch (kind) {
  'holder' => Holder.fromCbor(input).toCbor(),
  'contact' => Contact.fromCbor(input).toCbor(),
  'output' => LookupReadCountsOutput.fromCbor(input).toCbor(),
  _ => throw ArgumentError('unknown kind $kind'),
};

void main(List<String> args) {
  final failures = <String>[];
  for (final line in File(args[0]).readAsLinesSync()) {
    if (line.isEmpty || line.startsWith('#')) continue;
    final [name, kind, expect, input, canonical] = line.split('\t');
    Uint8List? encoded;
    Object? error;
    try {
      encoded = roundTrip(kind, unhex(input));
    } catch (e) {
      error = e;
    }
    if (expect == 'ok' && encoded == null) {
      failures.add('FAIL $name: decode failed: $error');
    } else if (expect == 'ok' && hex(encoded!) != canonical) {
      failures.add('FAIL $name: encoded ${hex(encoded)}, want $canonical');
    } else if (expect == 'err' && encoded != null) {
      failures.add('FAIL $name: decode accepted, encoded ${hex(encoded)}');
    }
  }
  failures.forEach(print);
  if (failures.isNotEmpty) exit(1);
  print('all wire vectors pass');
}
