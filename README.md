# Newt Lang
Newt is a domain specific language that compiles to Typescript type code. Newt
is not for general purpose programming, but rather for creating toys/abominations
in the TS type system.

## Language Guide


Fundamentally, Newt is designed to provide more ergonomics around conditional
types.

Take, for instance the following typescript expression:

```typescript
interface IdLabel {
  id: number
}

interface NameLabel {
  name: string
}

type IdOrNameLabel<T extends string | number> =
  T extends string 
  ? NameLabel 
  : IdLabel;

```

The same thing can be expressed in Newt using few different control structures:

```haskell
interface IdLabel {
  id: number
}

interface NameLabel {
  name: string
}

type IdOrNameLabel(T)
where T <: string | number
as
  if T <: string then
    NameLabel
  else
    IdLabel
  end
```

Or

```haskell
type IdOrNameLabel(T)
where T <: string | number
as
  cond do
    T <: string -> NameLabel
    else -> IdLabel
  end
```

Or finally:

```haskell
type IdOrNameLabel(T)
where T <: string | number
as
  match T do
    string -> NameLabel
    _ -> IdLabel
  end
```

While these simple examples probably aren't too compelling given all the extra
characters, this get much more useful as your type programs get more complicated.


## Development

Refer to `./mise.toml` for all build, dev, and test commands.
