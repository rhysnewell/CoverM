use bird_tool_utils::external_command_checker::*;

pub fn check_for_bwa() {
    check_for_external_command_presence("BWA", "which bwa");
}

pub fn check_for_minimap2() {
    check_for_external_command_presence("minimap2", "which minimap2");
    default_version_check("minimap2", "2.17-r941", false, None);
}

pub fn check_for_samtools() {
    check_for_external_command_presence("samtools", "which samtools");
    default_version_check("samtools", "1.9", false, None);
}
