# Source this file in Bash (including macOS Bash 3.2).
_split2flac() {
    local cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]} item
    local switches='-p -o -of -cue -cuecharset -nask -f -e -eh -enca -c -nc -C -nC -cs -d -nd -D -nD -F -colors -nocolors -g -ng -s -h -help --help -v --version'
    COMPREPLY=()
    case "$prev" in
        -f) COMPREPLY=( $(compgen -W 'flac m4a mp3 ogg opus wav' -- "$cur") ); return ;;
        -cuecharset) COMPREPLY=( $(compgen -W 'UTF-8 UTF-16LE UTF-16BE GBK GB18030 Big5 Shift_JIS windows-1251 windows-1252' -- "$cur") ); return ;;
        -o) while IFS= read -r item; do COMPREPLY+=("$item/"); done < <(compgen -d -- "$cur"); return ;;
        -of|-e|-enca|-C|-cs) return ;;
        -cue|-c) ;;
        *) if [[ $cur == -* ]]; then COMPREPLY=( $(compgen -W "$switches" -- "$cur") ); return; fi ;;
    esac
    while IFS= read -r item; do COMPREPLY+=("$item"); done < <(compgen -f -- "$cur")
}
complete -o filenames -F _split2flac split2flac split2m4a split2mp3 split2ogg split2opus split2wav
