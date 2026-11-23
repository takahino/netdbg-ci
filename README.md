# netdbg-ci

netdbg を Linux x64 で止められるか確かめる公開の置き場です。製品のリポジトリは [netdbg](https://github.com/takahino/netdbg) です。

## スパイク

Linux x64 で `libdbgshim.so` を開き、.NET 8 のコンソールを起動し、`Program.cs` の `STOP` の行にブレークポイントを置いて、そこで止まることまでを見ます。`ICorDebug` の vtable は、Windows の COM と同じ並び（先頭が `QueryInterface`）で呼んでいます。

```bash
bash spike/run.sh
```

成功すると、最後の行が `stopped at … Program.cs:<行> il=<オフセット>` になり、終了コードは 0 です。
