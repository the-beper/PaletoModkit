# Flathub

1. **Manifest** (`packaging/io.github.the_beper.PaletoModkit.yml`), in entrambi i moduli:
   - sostituire `type: dir` con `type: git`, `url: https://github.com/the-beper/paleto-modkit.git`, `tag: v1.1.0`, `commit: 8d2f366f338aceee51b8096af7eaeca4137d6103` (commit verificato: il tag `v1.1.0` punta a questo commit);
   - decidere su `--talk-name=org.freedesktop.Flatpak`: rimuoverlo (il codice mostra già un avviso senza il permesso) oppure giustificarlo nella PR. Non ho trovato conferme della regola nella documentazione Flathub, quindi verifica;
   - giustificare nella PR `--share=network`, `--filesystem=~/Games`, `/mnt`, `/media`, `/run/media` e le cartelle Steam, valutando i portali dove possibile.
2. **`nuget-sources.json`**: va generato con `packaging/flatpak-nuget-sources.sh` su una macchina con .NET 10 installato. Qui il tentativo è fallito con `no NuGet package found: the restore failed` perché `dotnet` non è presente.
3. **Verifica del build** con il nuovo manifest (`flatpak-builder`), poi:
   - `flatpak-builder-lint manifest packaging/io.github.the_beper.PaletoModkit.yml`
   - `appstreamcli validate --pedantic packaging/io.github.the_beper.PaletoModkit.metainfo.xml`
   Non ho potuto eseguirli: `flatpak`, `flatpak-builder`, `appstreamcli` e `flatpak-builder-lint` non sono installati qui.
4. **Dichiarazione AI** nella PR Flathub: il codice, la documentazione e il packaging sono stati generati con AI, con indicazione delle parti e della loro estensione. Va scritta da te.
5. **Testi della PR e dei commit** (titolo, descrizione, risposte ai revisori): scritti da te, senza AI. Non chiedere review ad agent AI.
6. **Licenze**: verificare la compatibilità di tutte le dipendenze e di CodeWalker (`THIRD_PARTY.md`) con GPL-3.0-or-later.
7. **Revisione umana del codice**: una persona che conosca il codice deve poter rispondere alle domande dei revisori su sicurezza e permessi.
8. **Sottomissione**:
   - fare il fork di `flathub/flathub` (senza copiare solo il branch di default);
   - creare un branch dal base `new-pr` con `io.github.the_beper.PaletoModkit.yml`, `flathub.json` e i due JSON delle sorgenti;
   - aprire la PR verso `new-pr` con titolo "Add io.github.the_beper.PaletoModkit".
9. **Dopo l'accettazione**: accettare l'invito al repo `flathub/io.github.the_beper.PaletoModkit` e verificare la proprietà dell'ID `io.github.*` dalla dashboard Flathub, loggandosi come `the-beper`.
