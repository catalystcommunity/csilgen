alias Csilgen.Generated.{Cbor, Stamp}

# Elixir's DateTime keeps microseconds.
digits = 6
[path] = System.argv()

failures =
  path
  |> File.read!()
  |> String.split("\n", trim: true)
  |> Enum.reject(&String.starts_with?(&1, "#"))
  |> Enum.flat_map(fn line ->
    [name, expect, precision, input, canonical] = String.split(line, "\t")
    decoded =
      try do
        {:ok, Stamp.from_cbor(Base.decode16!(input, case: :lower))}
      rescue
        e -> {:error, e}
      end

    want_ok = expect == "ok" and String.to_integer(precision) <= digits

    case {want_ok, decoded} do
      {true, {:ok, stamp}} ->
        got = Base.encode16(Cbor.encode(Stamp.to_cbor_value(stamp)), case: :lower)
        if got == canonical, do: [], else: ["#{name}: encoded #{got}"]

      {true, {:error, e}} ->
        ["#{name}: decode failed: #{Exception.message(e)}"]

      {false, {:ok, stamp}} ->
        ["#{name}: decode accepted #{inspect(stamp.at)}"]

      {false, {:error, _}} ->
        []
    end
  end)

Enum.each(failures, &IO.puts/1)
if failures != [], do: System.halt(1)
IO.puts("elixir: all timestamp vectors pass")
