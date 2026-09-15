# Binary templates

These 010 Editor Binary Templates are vendored from SweetScape's public
[template repository](https://www.sweetscape.com/010editor/repository/templates/) (fetched 2026-09-15 by `fetch_templates.py`).
`build.rs` packs every `*.bt` here into the program, which deploys them to
`~/.config/rat-commander/templates/`.

## License

The repository's [terms](https://www.sweetscape.com/companyinfo/terms.html) state:

> By submitting a script or template to the repository, you agree to release your
> file into the public domain. Other people may download your file and use it for
> any purpose, commercial or otherwise.

A few templates carry their own license or attribution notes in their headers,
which are kept intact; those files are marked in the table below. Not vendored:

- `010.bt` — category Syntax
- `Bash.bt` — category Syntax
- `Batch.bt` — category Syntax
- `CPP.bt` — category Syntax
- `CSS.bt` — category Syntax
- `CSV.bt` — category Syntax
- `CSharp.bt` — category Syntax
- `Diff.bt` — category Syntax
- `G.bt` — category Syntax
- `GO.bt` — category Syntax
- `Gcode.bt` — category Syntax
- `HTML.bt` — category Syntax
- `Inspector.bt` — category Inspector
- `InspectorDates.bt` — category Inspector
- `InspectorGUID.bt` — category Inspector
- `InspectorVarint.bt` — category Inspector
- `InspectorWithMP4DateTime.bt` — category Inspector
- `Java.bt` — category Syntax
- `JavaScript.bt` — category Syntax
- `MDS.bt` — GPLv3 header (incompatible with GPL-2.0-only)
- `P5R_TBL.bt` — includes missing p5r_enums.bt, p5r_structs.bt
- `PAS.bt` — category Syntax
- `PHP.bt` — category Syntax
- `PowerShell.bt` — category Syntax
- `Python.bt` — category Syntax
- `QBDI.bt` — category Syntax
- `SQL.bt` — category Syntax
- `ULP.bt` — category Syntax
- `VB.bt` — category Syntax
- `XML.bt` — category Syntax
- `Yara.bt` — category Syntax

## Templates

| File | Category | Version | Authors | Purpose | Notes |
|---|---|---|---|---|---|
| 7ZIP.bt | Archive | 0.1 | Richard Perrott | Parse 7-Zip archive files. |  |
| APF.bt | Archive | 1.0.1 | Hyper (linktr.ee/hyperbx) | Parse Vector Unit archive files from Hydro Thunder Hurricane. |  |
| AR.bt | Archive | 1.0 | SweetScape Software | Parse ar archives used for .a, .lib, |  |
| ARC.bt | Archive | 1.0 | Kevin O. Grover | Parse SEA ARC files |  |
| CAB.bt | Archive | 0.3 | Alex McDonnell | Template for Microsoft cabinet format files. |  |
| CFB.bt | Archive | 0.1 | chsdup | Microsoft Compound File Binary File Format. |  |
| GZip.bt | Archive | 1.3 | Tim "diff" Strazzere | Quick template for parsing GZip data/files. |  |
| LZ4.bt | Archive | 0.2 | Hanno Hugenberg | Template for LZ4 Framing Format. |  |
| ME01.bt | Archive | 1.0 | EntranceJew | Parse ME01 archive files. |  |
| MSPY_UDP.bt | Archive | 1.0 | liuxilu@github | For Windows 10 Microsoft PinYin User Defined Phrase Data Files |  |
| Nus3Audio.bt | Archive | 1.0 | jam1garner | Parse nus3 music archives. |  |
| OrochiDAT.bt | Archive | 0.2 | SeleDreams | Analyzing Silicon Studio's Orochi / Mizuchi Engine data archiving format |  |
| PAK.bt | Archive | 1.0 | shuax | Parsing chrome pak files. |  |
| RAR.bt | Archive | 8.1 | Alexander Sherman, Jiaxi Ren | Parse RAR archives including 2.x, 3.x, 5.x and SFX RAR files. |  |
| SeqBox.bt | Archive | 1.0 | Marco Pontello | Explore a SeqBox file container/archive. |  |
| ShpcAnim.bt | Archive | 1.0 | jam1garner | Parse shpc animation archives. |  |
| SquashFS.bt | Archive | 2.0 | Drake Madison | Parses the SquashFS compressed read-only file |  |
| TAR.bt | Archive | 1.0 | xiaozhuai | Parse tar files. |  |
| U8.bt | Archive | 1.0.2 | Hyper (linktr.ee/hyperbx) | Parse Nintendo U8 archives. |  |
| ZIP.bt | Archive | 2.5 | SweetScape Software | Parse ZIP archive files. |  |
| ZIPAdv.bt | Archive | 3.0 | SweetScape Software | Defines a template for |  |
| ZSTD.bt | Archive | 1.0 | Nordgaren | Parse zstd compressed blob frames |  |
| ADTS.bt | Audio | 0.3 | zhoubo | Parse AAC's ADTS(Audio Data Transport Stream) audio files. |  |
| AUD.bt | Audio | 1.0 | Matthias Mailänder | Westwood Studios .aud sound format |  |
| CDA.bt | Audio | 0.2 | Szabolcs Dávid | Simple CDA file template to read Audio CD header information. |  |
| FLAC.bt | Audio | 0.1 | zhoubo | This template is used to parse Free Lossless Audio Codec file |  |
| MIDI.bt | Audio | 1.4 | Jack Andersen | General MIDI sound file template. Complete with |  |
| MP3.bt | Audio | 1.2 | Ivan Getta | Parse an MP3 music file. |  |
| OGG.bt | Audio | 1.1 | George Woods | Parses the ogg container format. |  |
| SF2.bt | Audio | 1.3 | gocha | Defines a template for |  |
| SID.bt | Audio | 1.0 | Cesare Pizzi (@red5heep) | SID (SID file format used for SID tunes in the HVSC (Hhttp://hvsc.de)) |  |
| SKPSilk.bt | Audio | 1.0 | yangzhikai | Parse SKP_SILK file |  |
| Surge_Wavetable.bt | Audio | 1.0 | Kyle Crockett | Used for inspecting and editing Surge Synth wavetable files. |  |
| WAV.bt | Audio | 1.3 | SweetScape Software, Paulo Max Gil I Reis | Parse WAV audio files. |  |
| WAVAdv.bt | Audio | 1.1 | SweetScape Software plus submissions | Defines an advanced template for |  |
| 3DS.bt | CAD | 1.0 | ZiZi | Parse an 3D Studio MAX scene files |  |
| Blend.bt | CAD | 1.0 | seesaw | Parsing Blender file |  |
| FBX.bt | CAD | 0.1 | Fred31 (Pavel Sokov) | Reading binary .fbx (FilmBox) files structure. |  |
| GLB.bt | CAD | 0.3 | FourCinnamon0 | Parse glb Khronos 3D Files |  |
| LAS.bt | CAD | 0.7 | M. Nicke | Analysis of point-clouds in LAS-Format |  |
| Modo.bt | CAD | 1.4 | Gwynne Reddick (Original version), Simon Beetham (This version) | Reads all known chunks in Modo *.lxo, *.lxp. *.lxe and *.lxl 3D files as documented at |  |
| MS3D.bt | CAD | 1.0 | Mete Ciragan, sapper, Corey Nguyen | Parse MilkShape3D v1.8.5 Scene File |  |
| OrCAD_BRD.bt | CAD | 1.0 | RomanRom2 | Parse OrCAD v2.10 database for PCB files. |  |
| OrCAD_LIB.bt | CAD | 1.4 | L. Potjewijd | Analyse OrCad 3.20a library files. |  |
| OrCAD_SCH.bt | CAD | 1.3 | L. Potjewijd | Analyse drawings and blocks from |  |
| PCAD45.bt | CAD | 1.0 | RomanRom2 | Parse P-CAD 2.0-4.5 database PCB, PRT, SCH, SYM files |  |
| Realflow_Bin_Particles.bt | CAD | 1.0 | Simon Beetham | Reads Nextlimit Realflow BIN particle files as per .pdf delivered with Realflow |  |
| STL.bt | CAD | 1.3 | ZiZi | Parse an STL binary file containing 3D geometry (CAD). |  |
| BoltDB.bt | Database | 0.8 | Simon N. Thornton | Display DB files based on the Golang BOLTDB (https://github.com/boltdb/) |  |
| DBF.bt | Database | 0.2 | A Norman | Parses .dbf (database) format files. |  |
| Foxpro_memo.bt | Database | 1.0.0 | A. Auerswald | Parses files in Visual FoxPro Memo format (DCT, FPT, FRT, LBT, MNT, PJT, SCT, TBK, VCT) |  |
| Foxpro_tables.bt | Database | 1.0.0 | A. Auerswald | Parses files in Visual FoxPro table format (DBC, DBF, FRX, LBX, MNX, PJX, SCX, VCX). |  |
| fpa_dataset.bt | Database |  | MQB-coding | VAG MQB FPA (fahrprofilauswahl / driving profile) dataset parsing |  |
| MongoDBWireProtocol.bt | Database | 0.1 | Raymond Hulha | Analyze captured data from a MongoDB client request. |  |
| SQLite.bt | Database | 0.3 | Andrew McRae | SQLite data file |  |
| DjVu.bt | Document | 1.0 | Pavel Rusanov | Parse DjVu document files |  |
| DOC.bt | Document | 0.1 | Oto | Parse main sections of Microsoft Doc format. |  |
| MOBI.bt | Document | 2.3 | David W. Deley | Parse Amazon Kindle ebook mobipocket |  |
| ONE.bt | Document | 0.2 | Harli Aquino | OneNote File Format |  |
| PDF.bt | Document | 0.3.4 | Didier Stevens, Christian Mehlmauer, Peter Wyatt | Template for PDF (Portable Document Format) files. | header: no Copyright, public domain |
| ADF.bt | Drives | 1.11 | Volker Broemmel (VB), Howard Price (HP) | Detect block types of AmigaDOS disk images. |  |
| APFS.bt | Drives | 1.13 | Yogesh Khatri | Read APFS (Apple File System) structures | header: License :, MIT |
| D64.bt | Drives | 1.0 | Cesare Pizzi (@red5heep) | D64 (Image of physical 1541 disk) |  |
| Drive.bt | Drives | 3.2.1 | SweetScape Software, Benjamin Vernoux | Parse logical and physical drives including |  |
| ElTorito.bt | Drives | 1.1 | A Kochkov | View the file system headers in an El Torito bootable cd image. |  |
| Es65_floppy.bt | Drives | 0.1 | Robert Offner MSc. | Parses floppy images for an old (and quite obscure) es65 Computer. | header: COPYRIGHT NOTE |
| Ext4.bt | Drives | 1.0 | Martijn Bogaard & Niels van Dijkhuizen | Partial parser for ext4 filesystems with some backwards compatibility for ext2 and 3. |  |
| FLEX.bt | Drives | 0.1 | Robert Offner MSc. | Parses floppy and harddisk images for TSC FLEX. | header: COPYRIGHT NOTE |
| GPTHeader.bt | Drives | 1.0 | Satoshi Kinebuchi | Decode GPT (GUID Partition Table) Header information |  |
| Imagedisk.bt | Drives | 0.2 | Robert Offner MSc. | Parses Dave Dunfields Imagedisk |  |
| ISO.bt | Drives | 0.7 | Anton Kochkov, Richard Perrott | Parse the file system headers for ISO disk images, and print Directory tree. |  |
| LogFile.bt | Drives | 1.0 | Sabhya Raj Mehta (5h4rrK) | Develop a parsing template for Resilient File System LogFile |  |
| LUKS.bt | Drives | 1.1 | Daniel Correa | Template for LUKS (Linux Unified |  |
| MBR.bt | Drives | 1.6 | Christian Schaffalitzky, A.Babecki, Simon N. Thornton (datarecovery@eazimail.com) | Parse an MBR, including any GPT extensions |  |
| MFT.bt | Drives | 0.1 | Eric R. Zimmerman | Parse a file containing NTFS MFT FILE records. |  |
| PSV.bt | Drives | 1 | devnoname120 | Template for exploring .psv files (archived Vita games). |  |
| QCOW2.bt | Drives | 1.0 | Diego Braga | QEMU Copy-On-Write v2 format parser. |  |
| ReFS.bt | Drives | 1.2 | Konstanin Germanov | Microsoft ReFS |  |
| Retrodrive.bt | Drives | 1.1 | Robert Offner MSc. | Parses a Variety of old Floppy Images (FLEX, FDOS, CP/M, es65, CP68, DOS68, D64, D71, D81, ATR, Apple ][,...). | header: COPYRIGHT NOTE |
| ROMFS.bt | Drives | 0.3 | Jordan Milne | Template for parsing and reverse engineering |  |
| Samdisk.bt | Drives | 0.1 | Robert Offner MSc. | Parses Samdisk DSK Files |  |
| SinclairMicrodrive.bt | Drives | 1.1 | J Pass | Defines a template for parsing |  |
| SytosPlus.bt | Drives | 1.1 | Simon N. Thornton | Decode Sytos Plus Tape Image and (optionally) dump recovered files. |  |
| UImage.bt | Drives | 0.1 | AngelToms | Parsing uboot images. |  |
| Uniflex.bt | Drives | 0.2 | Robert Offner MSc. | Parses floppy and harddisk images for Uniflex. | header: COPYRIGHT NOTE |
| VDI.bt | Drives | 1.0 | Diego Braga | Oracle VirtualBox VDI format parser. |  |
| VHD.bt | Drives | 1.2 | lurker0ster | Microsoft VHD virtual disk format parser. |  |
| VHDX.bt | Drives | 1.0 | Konstantin Germanov | Microsoft VHDX virtual disk format parser. |  |
| VMDK.bt | Drives | 1.5 | R99K_CN, Simon N. Thornton (datarecovery@eazimail.com) | Map out VMDK (VM disks) and locates MBR & GPT partitions (if present) |  |
| VOL_VOL.bt | Drives | 5.1 | Anaty Rahamim Bar Kat | New Repository inspec blob of VOL_VOL |  |
| WOZ.bt | Drives | 1.0 | Antoine Neuenschwander | Parses WOZ disk images |  |
| DynamixelProtocol.bt | Electronics | 0.1 | James Newton of HDRobotic.com | Verify returned status data from Dynamixel XL-320 (and other) servoes. |  |
| EatonAPR48.bt | Electronics | 1.0 | Glaukon Ariston | Eaton APR48 power supply's EEPROM structure. |  |
| EDID.bt | Electronics | 1.2 | Rafael Vuijk | Template for EDID files (Extended |  |
| EVSB.bt | Electronics | 1.1 | Kip Leitner, Panasonic | Decomposes Video Symbol Stream Files for |  |
| EZTap_EZVIEW2.bt | Electronics | 0.1 | Benjamin Vernoux | Template for EZ-Tap EZView to EZVIEW2 .dat file. |  |
| Goclever.bt | Electronics | 1.1 | Artur Babecki | Template for the GOCLEVER GPS Navigation log format |  |
| Mifare1k.bt | Electronics | 1.1 | Ruben Boonen (b33f) | Mifare Classic 1k Structure parsing. |  |
| Mifare4k.bt | Electronics | 1.1 | Ruben Boonen (b33f) | Mifare Classic 4k Structure parsing. |  |
| MifareClassic1K.bt | Electronics | 1.0 | Haicaji | Analysis Mifare Classic 1K (S50) dump File |  |
| MifareUltralight.bt | Electronics | 1.0 | ceres-c | Mifare Ultralight structure parsing. |  |
| NTAG215.bt | Electronics | 1.0 | Giovanni Cammisa (gcammisa) | NTAG215 Structure parsing. |  |
| OscarItem.bt | Electronics | 1.1 | S Reno | Template for binary journal file used |  |
| Picolog_PLW.bt | Electronics | 0.1 | Benjamin Vernoux | Template for PicoTech Picologger .PLW file. |  |
| SRec.bt | Electronics | 1.1 | Mario Ghecea | Motorola S-REC format template. This should work |  |
| 3DSX.bt | Executable | 1.0.0 | Mas0n | Parse 3DSX format designed for homebrew applications on the 3DS. |  |
| CLI.bt | Executable | 0.1 | Joe Kirwin | Highlight Common Language Infrastructure Metadata Header fields. |  |
| COFF.bt | Executable | 0.2 | guage | Parse Common Object File Format files. |  |
| DOL.bt | Executable | 0.2 | M.W. | Template for parsing dol Files from the Wii and Gamecube. |  |
| ELF.bt | Executable | 2.6.7 | Anon, Tim "diff" Strazzere, DaeK, JamesT, HTC, Harli Aquino, redqx, caprinux | Decode the ELF format for both 32/64 bit in big/little |  |
| EXE.bt | Executable | 0.9.9 | xSpy, Peter Kankowski, SweetScape Software, mirar | Parse Windows executable exe, dll, and sys files. |  |
| MachO.bt | Executable | 1.10 | Tim "diff" Strazzere, Harli Aquino | Quick template for parsing Mach-o binaries, |  |
| XBE.bt | Executable | 1.0.0 | Hyper (linktr.ee/hyperbx) | Parse Xbox executable files. |  |
| XEX.bt | Executable | 1.0.5 | Hyper (linktr.ee/hyperbx) | Parse Xbox 360 executable files. |  |
| EOT.bt | Font | 1.1 | Neo (Jiepeng) Tan | Template for parsing the Embedded OpenType (EOT) font file format. |  |
| FNT.bt | Font | 0.2 | Anon | Parse windows .FNT font files |  |
| OpenType.bt | Font | 1.1 | Dwayne Robinson and Alex McDonnell of Cisco Systems | Displays hierarchy of an OpenType font. This |  |
| TTF.bt | Font | 0.9 | James Newton of massmind.org and Alex McDonnell of Cisco Systems | Template to parse TTF (TrueType) fonts. |  |
| ntv2-gsb.bt | GIS | 0.1 | M. Nicke | Analysis of NTv2-files (shift-grids for date transformations) in binary format gsb |  |
| SCW.bt | GIS | 0.5 | Vorono4ka | SC3D 3D file format template (Supercell). |  |
| SHP.bt | GIS | 1.5 | A Norman | Parses ESRI ShapeFiles. |  |
| SHX.bt | GIS | 1.1 | A Norman | Parses ESRI shx files. |  |
| ANIM.bt | Game | 0.3.0 | xXCooBloyXx, pavidloq (Fred-31) | Parse .anim files (3D animation) from Gameloft games. |  |
| ANM.bt | Game | 1.0 | DaniilSV | Parse 3d animation asset from League Of Legends. |  |
| Anno2070_RDM.bt | Game | 1.0 | Adrian Dale | Unpack Anno 2070 .rdm 3D model files. |  |
| Bioware_Aurora_Module.bt | Game | 1.1 | Narta Xaymar Dirks <info@xaymar.com> | Extracting and creating .mod files |  |
| BLP.bt | Game | 1.1.1 | Alastor Strix'Efuartus | Parses a Blizzard BLP(.blp) |  |
| BPS.bt | Game | 1.0 | David Shadoff | Parse BPS patch files. |  |
| CoH3Rec.bt | Game | 0.2.0 | Ryan Taylor | Parse Company of Heroes 3 replay files. |  |
| CP77_CR2W.bt | Game | 0.51 | alphaZomega | For parsing CyberPunk 2077 files |  |
| CZProfile.bt | Game | 1.1 | MuLLlaH9! | Counter-Strike: Condition Zero profile data parser |  |
| Doom_WAD.bt | Game | 1 | f7cjo | Parsing Doom 1/2 WAD files |  |
| Earth2150_MSH.bt | Game | 0.1 | arkezar | Template for Earth 2150 msh files |  |
| FDS.bt | Game | 2.0 | Alex Sadler | Decode information in FDS files (Famicom Disk System) |  |
| GBS.bt | Game | 0.3.1 | Devyatyi9 | Parser for Aonyx Software's OtterUI scene files https://github.com/ppiecuch/OtterUI |  |
| Gemfire_SNES.bt | Game | 1.2 | Feldherren | Written for Gemfire (U) [!].sfc on SNES (may work with other ROM versions), with help from DragonAtma. Reading and editing game and scenario starting data values. |  |
| GFS_RevergeLabs.bt | Game | 0.2 | Devyatyi9, 0xFAIL | Parser for Lab Zero Games's (Reverge Labs) package file format used in Z-Engine |  |
| GGPK.bt | Game | 1.0 | Maximilian Munchow | A template for the content.ggpk used in Path of Exile. |  |
| GNF.bt | Game | 0.9 | avan | Parse PS4 GNF texture file |  |
| GODEATER_RES.bt | Game | 1 | Yamato Nagasaki | To Read + Highlight Important RES Data |  |
| IGI1_BIT.bt | Game | 1.0.10 | Rotari Artiom | Parse IGI 1 terrain.bit files |  |
| IGI2_RES.bt | Game | 1.0 | Rotari Artiom | 'IGI 2: Covert Strike' Resource pack format |  |
| IGI2_SPR.bt | Game | 1.0 | Rotari Artiom | 'IGI 2: Covert Strike' Sprite and Picture format |  |
| IGI2_TEX.bt | Game | 1.1 | Rotari Artiom | 'IGI 2: Covert Strike' Texture format |  |
| IGI2_THM.bt | Game | 1.0 | Rotari Artiom | 'IGI 2: Covert Strike' Terrain Height Map |  |
| IGI2_TLM.bt | Game | 1.0 | Rotari Artiom | 'IGI 2: Covert Strike' Terrain Light Map |  |
| IGI2_TMM.bt | Game | 1.0 | Rotari Artiom | 'IGI 2: Covert Strike' Terrain Material Map |  |
| IGI2_WAV.bt | Game | 1.0 | Rotari Artiom | 'IGI 2: Covert Strike' game sound format |  |
| iNes.bt | Game | 1.1 | Alexandre Frigon | Get information from the nes emulator standard format iNes. |  |
| IPS.bt | Game | 1.0 | David Shadoff | Parse IPS patch files. |  |
| KeeperFX_SAV.bt | Game | 1.1 | jomalin (from KeeperKlan forum https://keeperklan.com/) | Parse savegame SAV files (fx1g000X.sav) from KeeperFX videogame ver 0.4.9.2762 |  |
| KnyttStoriesWorld.bt | Game | 1.0 | fe3dback@yandex.ru | Parse knytt stories world file (should by extracted from *.bin) |  |
| LEVEL.bt | Game | 0.1.1 | pavidloq (Fred-31), xXCooBloyXx | Supercell level format for their old game(-s). |  |
| Lineage2Replay.bt | Game | 1.1 | L2Repository | Lineage2Replay (only packets list) |  |
| LithTech_DAT.bt | Game | 0.0.2 | anonymous | describes the map file format from the lithtech game engine |  |
| LoL_WAD.bt | Game | 1.0 | DaniilSV | Parse WAD asset archive from League Of Legends. |  |
| M2.bt | Game | 2.2.6 | Alastor Strix'Efuartus + credits to all previous Authors of the Original M2 template (Sorry but I have no idea who the original authors are) | Parses a Blizzard M2(.m2) Compatible with 1.x.x up to 11.x.x (Tested on 1.12.1 / 2.4.3 / 3.3.5 / 7.0.1 / 8.0.1 / 9.0.1 / 9.2.0 / 10.0.0 / 11.0.0) |  |
| M3DT.bt | Game | 1.0.0 | Alastor Strix'Efuartus + RE team behind M3 wiki data | Parses a Blizzard M3(.m3) |  |
| Mesh.bt | Game | 0.1 | Tyloth (Cyno Studios) | For parsing Sins of a Solar Empire II model files |  |
| MW2REG-GBL-DOS.bt | Game | 1.0 | Mateo Gomez | To allow for easily reading and editing the MechWarrior 2: Ghost Bear's Legacy DOS save file. |  |
| NDS.bt | Game | 1.0 | gocha | Defines a template for | header: General Public License |
| O3D.bt | Game | 0.1 | Jakob Klein | Read OMSI (Omnibus Simulator) Models. |  |
| PCESAV.bt | Game | 1.0 | David Shadoff | Parse PC Engine *.SAV files, for holding game save data. |  |
| PCFXSAV.bt | Game | 1.0 | David Shadoff, SweetScape Software, Benjamin Vernoux | Parse PC-FX SAV files. |  |
| PIG.bt | Game | 0.1.0 | RED_EYE, xXCooBloyXx, pavidloq (Fred-31) | Parse .pig files (3D models) from Gameloft games. |  |
| PPF.bt | Game | 1.0 | David Shadoff | Parse PPF3.0 patch files. |  |
| Psychonauts_DFF.bt | Game | 1.0 | veydzh3r | DFF Psychonauts Font File |  |
| Quake3Arena_BSP.bt | Game | 1.0 | Raymond Hulha | 010 Editor Binary Template for Quake3 BSP files. |  |
| Quake3Arena_MD3.bt | Game | 1.0 | Raymond Hulha | 010 Editor Binary Template for Quake3 MD3 files. |  |
| SBM.bt | Game | 0.1.1 | pavidloq (Fred-31), xxcoobloyxx | Supercell binary model format for their old games. |  |
| SCcom.bt | Game | 1.2 | fourk (github.com/FourCinnamon0), DaniilSV (github.com/Daniil-SV) | SupercellSWF compressed assets. |  |
| SCTX.bt | Game | 1.0 | Vorono4ka | SCTX (Supercell Texture) file format template |  |
| SGA.MSB.bt | Game | 0.3 | 0xFAIL | Parses the level data animation format from levels.gfs, used in Z-Engine (SkullGirls). |  |
| SGI.MSB.bt | Game | 0.5 | 0xFAIL | Parses the level data index format from levels.gfs, used in Z-Engine (SkullGirls). |  |
| SGM.MSB.bt | Game | 0.8 | 0xFAIL | Parses the level data models format from levels.gfs, used in Z-Engine (SkullGirls). |  |
| SGS.MSB.bt | Game | 0.1 | 0xFAIL | Parses the level data shape format from levels.gfs, used in Z-Engine (SkullGirls). |  |
| SKIN.bt | Game | 1.1.2 | Alastor Strix'Efuartus | Parses a Blizzard Skin(.skin) |  |
| SND-WAV.bt | Game | 0.1 | Devyatyi9 | Parses the snd-wav container format, used in Z-Engine (SkullGirls). |  |
| SPR.MSB.bt | Game | 0.1 | 0xFAIL | Parses the sprite data format from sprites.gfs, used in Z-Engine (SkullGirls). |  |
| TazWantedDat.bt | Game | 1.1 | MuLLlaH9! | Taz Wanted dat file parser |  |
| TazWantedSav.bt | Game | 1.1 | MuLLlaH9! | Taz Wanted save file parser (TazWanted.sav) |  |
| TGR.bt | Game | 1.1 | Sceadu | Parse animation assets from Kohan: Ahriman's Gift |  |
| UE4Pak.bt | Game | 1.2 | LEaN | UE4 package file version 9 (>=UE4 4.25) |  |
| UnityMetadata.bt | Game | 0.2 | xia0 | Parse unity3d metadata file |  |
| UPS.bt | Game | 1.0 | David Shadoff | Parse UPS patch files. |  |
| WIME_RES.bt | Game | 4.1 | Aaron R. Willis and Pavel Reznicek | Read War in Middle Earth (WIME) resource files. |  |
| WMO.bt | Game | 1.2.0 | Alastor Strix'Efuartus | Parses a Blizzard ".WMO" (World Model Object) |  |
| Wwise_SoundBank.bt | Game | 0.1 | Alexander Lombardi | Parses the Wwise SoundBank container format, which contains .wem sound files. Wwise (Wave Works Interactive Sound Engine) is Audiokinetic's software for interactive media and video games. |  |
| ASE.bt | Image | 1.0 | Jack Humbert | Parse Adobe Color Palette Files |  |
| BMP.bt | Image | 2.6 | SweetScape Software, Jakob Klein | Parse BMP image files. |  |
| CRN.bt | Image | 1.2 | HearHellacopters | Header parsing for crunch texture files in .crn format. |  |
| DDS.bt | Image | 1.2 | Aaron Cooper | Parse DDS images |  |
| EMF.bt | Image | 0.4 | Dustin D. Trammell | Enhanced Metafile Format (EMF) template. |  |
| GIF.bt | Image | 1.4 | Berend-Jan "SkyLined" Wever | Defines a template for |  |
| ICNS.bt | Image | 1.0 | SweetScape Software | Apple Icon Image format. |  |
| ICO.bt | Image | 1.2 | Amotz Getzov, Swigger | Defines a template for |  |
| JPG.bt | Image | 1.7 | Ibrahim Onaran | Template for JPG image files. |  |
| PAL.bt | Image | 1.2 | SweetScape Software | Parses a Microsoft PAL palette file. |  |
| PCX.bt | Image | 0.2 | James Newton | Parse a PCX bitmap header. |  |
| PNG.bt | Image | 2.3 | Kevin O. Grover, RCS, Mister Wu | Parse PNG (Portable Network Graphics) and APNG (Animated Portable Network Graphics) image files. |  |
| PSD.bt | Image | 1.0 | Aurora | Photoshop PSD image format. |  |
| QOI.bt | Image | 1.0 | xiaozhuai | Parse QOI image files. |  |
| TGA.bt | Image | 1.1 | Chiuta Adrian Marius | Shows the fields of a TGA image. |  |
| TIF.bt | Image | 1.9 | Kevin O. Grover | Parse TIFF (Tagged Image File Format) files, including GeoTIFF. |  |
| Webp.bt | Image | 1.0 | HearHellacopters | Webp image format. |  |
| WMF.bt | Image | 0.4 | Didier Stevens | Parse the Windows Metafile (WMF) graphics file format. |  |
| SWF.bt | Internet | 2.0 | Josh Zelonis, JPEXS | Defines a template for parsing Adobe Flash SWF files |  |
| Torrent.bt | Internet | 1.40 | Bartosz Dziewonski | Parse torrent files. |  |
| WASM.bt | Internet | 0.2 | Harli Aquino | WebAssembly (WASM) Template | header: License:, public domain |
| msgpack.bt | Machine Learning | 1.0 | Alexander Salas Bastidas <a.salas@ieee.org> | MessagePack serialization format for ML models and data |  |
| 2bit.bt | Medical | 1.0 | Andrew Sutton | Decode the UCSC 2bit genome file format |  |
| DICOM.bt | Medical | 0.03 | Andrew Brooks | DICOM medical imaging format. |  |
| SCF.bt | Medical | 0.1 | Matthias Mailänder | read SCF gene sequence trace files |  |
| BTCBlock.bt | Misc | 1.5 | Larry Friedman | Decode Bitcoin Core block |  |
| FTS.bt | Misc | 1.0 | Dmitry Trefilov | Microsoft Exchange FTS (Fast Transfer Stream) format template. |  |
| KMX.bt | Misc | 1.0 | Marc Durdin | Keyman compiled keyboard .kmx file format based on headers and source |  |
| KryoFlux.bt | Misc | 0.2 | Vasyl Tsvirkunov | Kryoflux Stream file template. |  |
| Notepad-TabState.bt | Misc | 0.2 | ogmini (https://github.com/ogmini), NordGaren (https://github.com/nordgaren/) | Template to make sense of the Tab State file for Windows 11 Notepad |  |
| Notepad-WindowState.bt | Misc | 0.2 | ogmini https://github.com/ogmini | Template to make sense of the Window State file for Windows 11 Notepad |  |
| PB.bt | Misc | 1.0 | shuax | Parsing google protocol buffers format. |  |
| RDB.bt | Misc | 1.1 | AnTler | Defines a template for parsing QQ's RDB files. |  |
| RIFF.bt | Misc | 1.1 | gocha | Defines a template for |  |
| SC.bt | Misc | 1.5 | Vorono4ka | SWF file format template (Supercell). This file format is compressed by lzma or lzham! Decompress it for use this template |  |
| SC2.bt | Misc | 1.2 | Vorono4ka | SC2 file format template (Supercell) |  |
| SCP.bt | Misc | 0.3 | Vasyl Tsvirkunov | SuperCard Pro dump file format |  |
| Tacx.bt | Misc | 1.1 | Nestor Matas | Template for all the Tacx Fortius cycling training |  |
| TradeActivityLog.bt | Misc | 1.0 | George Tarantilis | Parse Sierra Chart Trade Activity Log files. |  |
| TXD.bt | Misc | 1.0 | shuax | Parsing Tencent QQ txd or gmd files. |  |
| UMSE.bt | Misc | 0.1 | David Alvarez Perez | Template for Universal Malware Sample Encryption |  |
| ASTERIX.bt | Network | 0.3 | Kevin O. Grover | Eurocontrol ASTERIX Data |  |
| NetflowVersion5.bt | Network | 1.1 | Andrew Faust | Parses Cisco's Netflow Version 5 format. |  |
| PCAP.bt | Network | 0.6 | Didier Stevens | Parse a PCAP network capture file. |  |
| PCAPNG.bt | Network | 1.4 | Kevin O. Grover | Parse a PCAPNG Packet Capture file. |  |
| SSP.bt | Network | 0.2 | ThangCuAnh (TQN) - HVA | Define a template for parsing SmartSniff Packet files. |  |
| TLS_ClientHello.bt | Network | 1.0 | Raymond Hulha | Parse TLS ClientHello Message |  |
| TNEF.bt | Network | 0.2 | Harli Aquino | Transport Neutral Encapsulation Format (TNEF) Template (usually for Winmail.dat or Win.dat). |  |
| OMF51.bt | Object File | 1.7 | Galen Tackett, with assistance from claud.ai | Parse Intel OMF-51 object files with ARM Keil extensions |  |
| Abc.bt | Operating System | 1.1 | hx1997 | 010Editor template for .abc (Open/HarmonyOS Ark Bytecode) files |  |
| Abc12.bt | Operating System | 1.3 | hx1997 | 010Editor template for .abc (Open/HarmonyOS Ark Bytecode) files version >=12.0.1.0. For older versions see Abc.bt. |  |
| AndroidBoot.bt | Operating System | 3.8 | Bjoern Kerler, Shaohua Xia, feicong | Android boot image template |  |
| AndroidManifest.bt | Operating System | 1.3.0 | dongmu | Define a template for parsing |  |
| AndroidOtaPayload.bt | Operating System | 1.0 | TrustKernel | Parse payload.bin in Android OTA package |  |
| AndroidPersistentProperties.bt | Operating System | 1.0 | feicong | parse android /data/property/persistent_properties (Android 8+) |  |
| AndroidResource.bt | Operating System | 1.2 | lichao, fei_cong, hj | Parse AndroidManifest.xml, res/*.xml, and resource.arsc |  |
| AndroidTrace.bt | Operating System | 1.0 | Banny | Define a template for parsing dmtrace.trace files. |  |
| AndroidVBMeta.bt | Operating System | 1.4 | Bjoern Kerler | Android vbmeta partition template |  |
| BPlist.bt | Operating System | 1.0.1 | Alexey Lyashko | Template for parsing Apple Binary Property List format. |  |
| Cryptfs.bt | Operating System | 1.3 | Tim 'diff' Strazzere, Bjoern Kerler | Parse the Cryptfs footer from encrypted drives (specifically Android) |  |
| DEX.bt | Operating System | 2.2.0 | Jon Larimer, Tim Strazzere | A template for analyzing Dalvik VM | header: License:, public domain |
| DHTB.bt | Operating System | 1.2 | B. Kerler | Spreadtrum/Unisoc  Container |  |
| DMP.bt | Operating System | 1.2 | A Schuster | Template to parse the header of a |  |
| DS_Store.bt | Operating System | 0.5 | Aurora | Apple .DS_Store files. Stores file attributes. |  |
| DTB.bt | Operating System | 2.0 | marv7000 | OpenFirmware device tree blob |  |
| FUTX.bt | Operating System | 0.2 | Pascal Mathis | Interpret user accounting information exposed by FreeBSD |  |
| HFSJournal.bt | Operating System | 1.0 | blukat29 | Parse an HFS+ (HFS Plus file system) journal file. |  |
| Hisi_Sec.bt | Operating System | 1.0 | Bjoern Kerler | Parse Huawei Tee Sec Encrypted Trustlets V3.01 |  |
| IconCache.bt | Operating System | 1.0 | Phill Moore, Yogesh Khatri | Read %localappdata%\IconCache.db files |  |
| IntelFramebuffer.bt | Operating System | 0.6 | vit9696 | Intel Framebuffer decoding. |  |
| JL_Auto.bt | Operating System | 0.1 | Hyesun Jang, Yukyeong Lee | Parse Microsoft JumpList-automatic(*.automaticDestinations-ms) |  |
| JL_Custom.bt | Operating System | 0.1 | Hyesun Jang, Yukyeong Lee | Parse Microsoft JumpList-custom(*.customDestinations-ms) |  |
| LNK.bt | Operating System | 0.5 | Didier Stevens, Trevor Welsby | View data in a Microsoft shortcut (LNK) file. | header: no Copyright, public domain |
| MIBIB.bt | Operating System | 1 | Nikita F. | qcom mibib part parser |  |
| MiniDump.bt | Operating System | 1.0 | darknesswind | Template parse struct in Windows MiniDump. |  |
| MTK_MCLF.bt | Operating System | 1.0 | Bjoern Kerler | Mediatek Trustonic Trustlet Parser |  |
| MTK_TEE.bt | Operating System | 1.5 | Bjoern Kerler | Mediatek Tee Parser |  |
| NLS.bt | Operating System | 1.0 | HTC - VinCSS (a member of Vingroup) | Dump NLS content file C_xxxxx.nls |  |
| OpenWRT-BIN.bt | Operating System | 1.2 | Simon N. Thornton | Decode OpenWRT MIPS BIN images. |  |
| PF.bt | Operating System | 0.3 | Changhwan Ji, Hyunjin Kim, Heo Songyi, Simon N. Thornton | Quick template for parsing Windows Prefetch files (*.pf) |  |
| PSF.bt | Operating System | 1.1 | Boris Mazic | Parses a WSUS PSF (Patch Storage File) file. |  |
| RegistryDhcpInterfaceOptions.bt | Operating System | 1.0 | Simon N. Thornton | Decode the "DhcpInterfaceOptions" registry entry |  |
| RegistryHive.bt | Operating System | 1.6 | Eric R. Zimmerman, Yogesh Khatri, ogmini | Parses Windows Registry hive structures. Includes Header, |  |
| RegistryPolicyFile.bt | Operating System | 1.1 | Blake Frantz | Template for Windows registry policy files (registry.pol). |  |
| Samsung_PIT.bt | Operating System | 1.0 | B. Kerler | Parse Samsung PIT file (Partition Information Table) |  |
| SonyXperiaSINv3.bt | Operating System | 1.0 | Ismael034, Thx @zxz0O0 and @Androxyde | Sony Xperia SIN partition table |  |
| ThumbCache.bt | Operating System | 1.0 | Denis Anisimov | Parses Windows thumbnail cache files (thumbcache_idx.db, thumbcache_XXX.db). |  |
| UF2.bt | Operating System | 1.0 | Ronan Loftus | Parsing USB Flashing Format (UF2) images |  |
| UTMP.bt | Operating System | 0.2 | Matthew Geiger | Interpret entries in utmp and wtmp login |  |
| BSON.bt | Programming | 0.2 | Chris Russell (Gnorizo), Fabio Napoli | Parse BSON (binary JSON) files, per v1.0 spec at http://bsonspec.org/spec.html. Used by MongoDB et al. |  |
| BTF.bt | Programming | 1.0 | feicong | parse eBPF BTF info. |  |
| CAP.bt | Programming | 1.1 | Agus Purwanto | Parse java card CAP file with bytecode |  |
| CLASS.bt | Programming | 1.5 | Kevin O. Grover | Parse Java Class (JVM) files. |  |
| CLASSAdv.bt | Programming | 1.3 | Pishchik Ilya L. (RUS) | A template for parsing Java Class (JVM) Files. |  |
| JavaSerializationStream.bt | Programming | 1.0 | Ovie | Parse Java Object Serialization Stream |  |
| JSC.bt | Programming | 1.0.0 | hluwa | Parse a compiled JavaScript file of form SpiderMonkey_v52. |  |
| Luac.bt | Programming | 1.1 | fei_cong | Parse lua bytecode .lua and .luac files, support lua 5.2. | header: License:, public domain |
| LuaJIT.bt | Programming | 1.1 | feicong | Parse luajit bytecode files, support luajit 2.0.5. | header: License:, public domain |
| OPCache.bt | Programming | 0.1 | Ian Bouchard | Parse cache files generated by OPcache for PHP files on an x86 platform. | header: License:, MIT |
| PYC.bt | Programming | 1.2 | Lao Lao | Parse python bytecode .pyc and .pyo files,support python 1.5 to 3.13. |  |
| RES.bt | Programming | 1.1 | Sergey Evtushenko | Parses resources structure of a RES file or a PE file with the .rsrc section. |  |
| nt_mdt.bt | Scientific | 1.0 | Alexander Salas Bastidas <a.salas@ieee.org> | NT-MDT scientific data format for scanning probe microscopy |  |
| specpr.bt | Scientific | 1.0 | Alexander Salas Bastidas <a.salas@ieee.org> | SPECPR spectroscopy data format for spectrum processing |  |
| PAC.bt | Security | 1.0 | Volodymyr Khomenko | Kerberos PAC - KERB_VALIDATION_INFO |  |
| 010Theme.bt | Software | 0.0.1 | Michael Appel | Template for 010 Editor color/theme export files. | header: No Copyright |
| CRX.bt | Software | 1.1 | G Beier | Template for packaged Google Chrome |  |
| HiewCMarkers.bt | Software | 1.0 | Jupiter | Hiew Colour Markers highlighting. |  |
| TOC.bt | Software | 1.3 | L. Potjewijd | Template to make sense of, and modifications to, |  |
| WinhexPos.bt | Software | 1.1 | Artur Babecki | The WinHex (editor by X-Ways Software Technology AG) |  |
| ASF.bt | Video | 0.7 | scigrapher | To parse Advanced Systems Format files, which include .asf, .wma, and .wmv |  |
| AVI.bt | Video | 1.2 | Blaine Lefebvre [bl], Elias Bachaalany [eb] | Parse an AVI movie file. |  |
| BaseMedia.bt | Video | 1.1 | @RReverser | Parse ISO Base Media File Format files |  |
| EBML.bt | Video | 0.8 | scigrapher | To parse Extensible Binary Meta Language (EBML) files, which includes mkv and webm |  |
| FLV.bt | Video | 3.0 | zozobreak@163.com | Template for Flash Video (FLV) files including sei and nalu. |  |
| H264.bt | Video | 0.7 | ZJX, Radu Arjocu | Identify NAL units of an AVC/H264 video stream, which uses start codes. |  |
| MP4.bt | Video | 3.4 | Alexey Lyashko, Andrew Molyneux, Marian Denes, SweetScape, Marko Musa | Defines a template for parsing MP4 and MOV video files. |  |
| MXF.bt | Video | 0.2 | Stefan Riediger | Parse MXF files (Material Exchange Format, SMPTE 377M, SMPTE EG41, SMPTE EG42) |  |
| RM.bt | Video | 1.0 | Jian Xu | Defines a template for parsing RM (RealMedia) video files. |  |
| TS.bt | Video | 0.2 | zhoubo | Parse Transport Stream. Support parse TS,PAT,PMT and parts of PES. |  |
