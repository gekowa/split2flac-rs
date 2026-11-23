# Dot-source this file in PowerShell: . .\completions\split2flac.ps1
$split2Names = 'split2flac','split2m4a','split2mp3','split2ogg','split2opus','split2wav'
Register-ArgumentCompleter -Native -CommandName ($split2Names + @($split2Names | ForEach-Object { "$_.exe" })) -ScriptBlock {
    param($wordToComplete, $commandAst, $cursorPosition)
    $previous = $commandAst.CommandElements | Where-Object { $_.Extent.EndOffset -le ($cursorPosition - $wordToComplete.Length) } | Select-Object -Last 1
    $switches = '-p','-o','-of','-cue','-cuecharset','-nask','-f','-e','-eh','-enca','-c','-nc','-C','-nC','-cs','-d','-nd','-D','-nD','-F','-colors','-nocolors','-g','-ng','-s','-h','-help','--help','-v','--version'
    $choices = switch -CaseSensitive ($previous.Extent.Text) {
        '-f' { 'flac','m4a','mp3','ogg','opus','wav'; break }
        '-cuecharset' { 'UTF-8','UTF-16LE','UTF-16BE','GBK','GB18030','Big5','Shift_JIS','windows-1251','windows-1252'; break }
        { $_ -cin '-e','-of','-enca','-C','-cs' } { return }
        default {
            if ($wordToComplete.StartsWith('-')) { $switches; break }
            $paths = [System.Management.Automation.CompletionCompleters]::CompleteFilename($wordToComplete)
            if ($previous.Extent.Text -ceq '-o') { $paths = $paths | Where-Object { $_.ResultType -eq 'ProviderContainer' } }
            $paths; break
        }
    }
    foreach ($choice in $choices) {
        if ($choice -is [System.Management.Automation.CompletionResult]) { $choice; continue }
        if ($choice.StartsWith($wordToComplete, [StringComparison]::Ordinal)) {
            [System.Management.Automation.CompletionResult]::new($choice,$choice,'ParameterValue',$choice)
        }
    }
}
Remove-Variable split2Names
