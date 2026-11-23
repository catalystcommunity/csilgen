alias Csilgen.Generated.{Cbor, Moment}

# Elixir's DateTime keeps microseconds.
digits = 6
[path] = System.argv()

rows =
  path
  |> File.read!()
  |> String.split("\n", trim: true)
  |> Enum.reject(&String.starts_with?(&1, "#"))
  |> Enum.map(&String.split(&1, "\t"))
  |> Enum.filter(fn [_, _, d, _, _] -> d == "*" or String.to_integer(d) == digits end)

failures =
  Enum.flat_map(rows, fn [name, expect, _, input, canonical] ->
    decoded =
      try do
        {:ok, Moment.from_cbor(Base.decode16!(input, case: :lower))}
      rescue
        e -> {:error, e}
      end

    case {expect == "ok", decoded} do
      {true, {:ok, moment}} ->
        got = Base.encode16(Cbor.encode(Moment.to_cbor_value(moment)), case: :lower)
        if got == canonical, do: [], else: ["FAIL #{name}: re-encoded #{got}, want #{canonical}"]

      {true, {:error, e}} ->
        ["FAIL #{name}: decode failed: #{Exception.message(e)}"]

      {false, {:ok, moment}} ->
        ["FAIL #{name}: decoded, but must fail (#{inspect(moment.at)})"]

      {false, {:error, _}} ->
        []
    end
  end)

Enum.each(failures, &IO.puts/1)

if failures != [] do
  IO.puts("#{length(failures)} of #{length(rows)} epoch vector(s) failed")
  System.halt(1)
end

IO.puts("all #{length(rows)} epoch vectors pass")
