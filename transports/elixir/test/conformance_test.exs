defmodule Csilgen.Transport.Vectors do
  @moduledoc false
  # Compiled before the test module so the module-level `for vec <- load(...)`
  # comprehensions can call it: a module cannot invoke its own functions while it
  # is still being compiled, so the loader has to live in a separate module.
  @conformance_dir Path.join(__DIR__, "../../conformance")

  def load(name) do
    @conformance_dir
    |> Path.join(name)
    |> File.read!()
    |> JSON.decode!()
    |> Map.fetch!("vectors")
  end
end

defmodule Csilgen.Transport.ConformanceTest do
  @moduledoc """
  Verify the library against the checked-in, byte-exact conformance vectors —
  the contract every transport library follows. For each vector: rebuild the
  envelope from its language-neutral `input`, assert encode → `hex`, and assert
  decode(`hex`) → the same envelope. Ported from
  transports/rust/tests/conformance.rs and transports/go/conformance_test.go.

  JSON is parsed with the OTP-native `JSON` module (Elixir 1.18+/OTP 27+), so no
  Jason dependency is needed; hex uses stdlib `Base`.
  """

  use ExUnit.Case, async: true

  alias Csilgen.Transport.{Datagrams, Events, RPC, Status}
  import Csilgen.Transport.Vectors, only: [load: 1]

  defp unhex(nil), do: <<>>
  defp unhex(""), do: <<>>
  defp unhex(hex), do: Base.decode16!(hex, case: :lower)

  defp tohex(bytes), do: Base.encode16(bytes, case: :lower)

  # JSON `null` (and an absent key) both mean ABSENT for an optional field.
  defp opt(input, key), do: Map.get(input, key)

  describe "rpc vectors" do
    for vec <- load("rpc.json") do
      @vec vec
      test "rpc/#{vec["name"]}" do
        input = @vec["input"]
        payload = unhex(input["payload_hex"])

        {actual, decoded, expected_env} =
          case input["kind"] do
            "request" ->
              env = %RPC.Request{
                service: input["service"],
                op: input["op"],
                id: opt(input, "id"),
                auth: opt(input, "auth"),
                payload: payload
              }

              {:ok, dec} = RPC.decode_request(RPC.encode_request(env))
              {RPC.encode_request(env), dec, env}

            "response" ->
              env = %RPC.Response{
                id: opt(input, "id"),
                status: input["status"],
                variant: opt(input, "variant"),
                error: opt(input, "error"),
                payload: payload
              }

              {:ok, dec} = RPC.decode_response(RPC.encode_response(env))
              {RPC.encode_response(env), dec, env}

            "push" ->
              env = %RPC.Push{
                service: input["service"],
                event: input["event"],
                payload: payload
              }

              {:ok, dec} = RPC.decode_push(RPC.encode_push(env))
              {RPC.encode_push(env), dec, env}
          end

        assert tohex(actual) == @vec["hex"]
        assert decoded == expected_env
      end
    end
  end

  describe "events vectors" do
    for vec <- load("events.json") do
      @vec vec
      test "events/#{vec["name"]}" do
        input = @vec["input"]

        case input["control"] do
          nil ->
            payload = unhex(input["payload_hex"])
            {:ok, profile} = Events.parse_profile(input["profile"])

            env =
              case profile do
                :verbose ->
                  %Events.Event{
                    service: opt(input, "service"),
                    event: input["event"],
                    id: opt(input, "id"),
                    payload: payload
                  }

                :compact ->
                  %Events.Event{
                    service_ord: input["service_ord"],
                    op_ord: input["op_ord"],
                    id: opt(input, "id"),
                    payload: payload
                  }
              end

            actual = Events.encode_event(env, profile)
            assert tohex(actual) == @vec["hex"]
            {:ok, decoded} = Events.decode_event(actual, profile)
            assert decoded == env

          control ->
            actual = encode_control(control, input)
            assert tohex(actual) == @vec["hex"]
        end
      end
    end
  end

  defp encode_control("hello", input) do
    Events.encode_hello(%Events.Hello{
      versions: input["versions"],
      profiles: input["profiles"],
      service: opt(input, "service"),
      auth: opt(input, "auth")
    })
  end

  defp encode_control("hello_ack", input) do
    Events.encode_hello_ack(%Events.HelloAck{
      v: input["v"],
      profile: input["profile"],
      session: opt(input, "session")
    })
  end

  defp encode_control("ping", input) do
    Events.encode_heartbeat(%Events.Heartbeat{nonce: input["nonce"], at: opt(input, "at")})
  end

  defp encode_control("close", input) do
    Events.encode_close(%Events.Close{status: input["status"], reason: opt(input, "reason")})
  end

  describe "datagram vectors" do
    for vec <- load("datagrams.json") do
      @vec vec
      test "datagrams/#{vec["name"]}" do
        input = @vec["input"]

        case input["profile"] do
          "cbor-array" ->
            env = %Datagrams.Datagram{
              op_ord: input["op_ord"],
              seq: input["seq"],
              payload: unhex(input["payload_hex"])
            }

            actual = Datagrams.encode_datagram(env)
            assert tohex(actual) == @vec["hex"]
            {:ok, decoded} = Datagrams.decode_datagram(actual)
            assert decoded == env

          "compact-header" ->
            base = %Datagrams.CompactDatagram{
              op_ord: input["op_ord"],
              seq: input["seq"],
              body: unhex(input["body_hex"])
            }

            env =
              case opt(input, "epoch") do
                nil -> base
                e -> Datagrams.with_epoch(base, e)
              end

            actual = Datagrams.encode_compact(env)
            assert tohex(actual) == @vec["hex"]
            {:ok, decoded} = Datagrams.decode_compact(actual)
            assert decoded == env
        end
      end
    end
  end

  test "status registry names round-trip the registry codes" do
    assert Status.name(Status.ok()) == "ok"
    assert Status.name(Status.unknown_service_or_op()) == "unknown-service-or-op"
    assert Status.name(99) == "other"
    assert Status.ok?(Status.ok())
    refute Status.ok?(Status.internal())
  end
end
