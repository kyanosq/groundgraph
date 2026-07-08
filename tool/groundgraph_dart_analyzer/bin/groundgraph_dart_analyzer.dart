/// Entry point for the GroundGraph Dart analyzer sidecar.
///
/// Usage:
///   dart run groundgraph_dart_analyzer --request /path/to/request.json
///   echo '{"repo_root":"…","code_roots":["lib","test"]}' | dart run …
///
/// On success, writes a `SidecarBatchResponse` JSON object to stdout and
/// exits 0. On a recoverable failure, writes a `SidecarErrorResponse` to
/// stdout and exits 0 anyway — the Rust engine decides whether to fall
/// back. On an unrecoverable failure (bad request, etc.), writes the
/// error response and exits 2.
library;

import 'dart:convert';
import 'dart:io';

import 'package:args/args.dart';

import 'package:groundgraph_dart_analyzer/protocol.dart';
import 'package:groundgraph_dart_analyzer/walker.dart';

Future<int> main(List<String> argv) async {
  final parser = ArgParser()
    ..addOption(
      'request',
      abbr: 'r',
      help: 'Path to a JSON file containing the SidecarRequest. If absent,'
          ' the sidecar reads JSON from stdin.',
    )
    ..addFlag('help', abbr: 'h', negatable: false);
  ArgResults args;
  try {
    args = parser.parse(argv);
  } on FormatException catch (e) {
    stdout.writeln(jsonEncode(SidecarErrorResponse(
      code: 'bad_arguments',
      message: e.message,
    ).toJson()));
    return 2;
  }
  if (args['help'] as bool) {
    stdout.writeln('Usage: groundgraph_dart_analyzer [--request FILE]');
    return 0;
  }

  Map<String, dynamic> requestJson;
  try {
    final raw = args['request'] != null
        ? await File(args['request'] as String).readAsString()
        : await _readAllStdin();
    if (raw.trim().isEmpty) {
      stdout.writeln(jsonEncode(SidecarErrorResponse(
        code: 'empty_request',
        message: 'no JSON provided on --request or stdin',
      ).toJson()));
      return 2;
    }
    requestJson = jsonDecode(raw) as Map<String, dynamic>;
  } catch (e) {
    stdout.writeln(jsonEncode(SidecarErrorResponse(
      code: 'bad_request',
      message: 'failed to read or parse request JSON',
      // #103: a PathNotFoundException embeds the absolute request path.
      detail: sanitizeDiagnosticText('$e'),
    ).toJson()));
    return 2;
  }

  late SidecarRequest req;
  try {
    req = SidecarRequest.fromJson(requestJson);
  } catch (e) {
    stdout.writeln(jsonEncode(SidecarErrorResponse(
      code: 'bad_request_shape',
      message: 'request JSON does not match SidecarRequest contract',
      detail: sanitizeDiagnosticText('$e'),
    ).toJson()));
    return 2;
  }

  try {
    final batch = await walkRepository(req);
    stdout.writeln(jsonEncode(batch.toJson()));
    return 0;
  } catch (e, st) {
    // #103: the stack trace embeds absolute paths (…/Users/<name>/…) that would
    // otherwise land in the graph's diagnostics + HTML reports. Keep the full
    // trace on stderr (local debug channel) and serialise only the sanitised
    // exception message.
    stderr.writeln('walker_failed: $e\n$st');
    stdout.writeln(jsonEncode(SidecarErrorResponse(
      code: 'walker_failed',
      message: 'analyzer walker threw an exception',
      detail: sanitizeDiagnosticText('$e'),
    ).toJson()));
    return 0; // Let the engine fall back gracefully.
  }
}

Future<String> _readAllStdin() async {
  final chunks = <int>[];
  await for (final chunk in stdin) {
    chunks.addAll(chunk);
  }
  return utf8.decode(chunks);
}
