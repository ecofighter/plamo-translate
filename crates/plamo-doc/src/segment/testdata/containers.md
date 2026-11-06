# Containers

> Quoted paragraph before some HTML.
>
> <div>
> quoted html text
> </div>
>
> Quoted paragraph after the HTML.

> A quote that holds a list:
>
> - quoted item with code:
>
>   ```
>   quoted list code
>   ```
>
> - second quoted item
>
> [qdef]: https://x.y/q "Quoted definition"
>
> Uses [quoted][qdef] here.

- list item text

  [ldef]: https://x.y/l "List definition"
- uses [listed][ldef] here

| Command | Meaning |
|---|---|
| `a\|b` | pipe inside inline code |
| `c` | plain cell |

Footnote reference.[^n]

[^n]: Footnote paragraph.

    ```
    footnote code
    ```

    - footnote list item

    [fdef]: /f "Footnote definition"

See the collapsed image ![logo][] and [fdef].

[logo]: /logo.png
