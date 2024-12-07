cmake_minimum_required(VERSION 3.16)

project(msquic-asio LANGUAGES)

set(_msquic_version "v2.4.5")
set(_win_pkg_url "https://github.com/microsoft/msquic/releases/download/${_msquic_version}/msquic_windows_x64_Release_schannel.zip")
set(_linux_pkg_url "https://github.com/microsoft/msquic/releases/download/${_msquic_version}/msquic_linux_x64_Release_openssl.zip")
if(WIN32)
  set(release_url ${_win_pkg_url})
else()
  # linux
  set(release_url ${_linux_pkg_url})
  find_package(OpenSSL REQUIRED)
  # openssl needs version 1.1. ubuntu 20.04 has it, 22.04 is too new.
endif(WIN32)

message(STATUS "fetching msquic_release " ${release_url})
include(FetchContent)
FetchContent_Declare(
  msquic_release
  DOWNLOAD_EXTRACT_TIMESTAMP true
  URL ${release_url}
)
FetchContent_MakeAvailable(msquic_release)

# fetch linux pkg as well on windows for binding generation
if(WIN32)
message(STATUS "fetching msquic_linux_release " ${_linux_pkg_url})
FetchContent_Declare(
  msquic_linux_release
  DOWNLOAD_EXTRACT_TIMESTAMP true
  URL ${_linux_pkg_url}
)
FetchContent_MakeAvailable(msquic_linux_release)
endif(WIN32)

if(WIN32)
# generate winmd. defer generation task in .metadata csproj.
find_program (
    dotnet_exe
    NAMES dotnet.exe
    REQUIRED
)

set(_out_winmds
  Microsoft.MsQuic.winmd
  Windows.Win32.winmd
)
set(_out_winmd_paths "")
foreach(_out_winmd ${_out_winmds})
list(APPEND _out_winmd_paths ${CMAKE_CURRENT_SOURCE_DIR}/.windows/winmd/${_out_winmd})
endforeach()

add_custom_command(
  OUTPUT ${_out_winmd_paths}
  COMMAND ${dotnet_exe} build
  WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}/.metadata
  VERBATIM
)

add_custom_target(generate_winmd
  DEPENDS ${_out_winmd_paths}
)

find_program(cargo_exe
  NAMES cargo
  REQUIRED
)

add_custom_target(generate_rust
  COMMAND ${cargo_exe} run -p tools_api
  WORKING_DIRECTORY ${CMAKE_CURRENT_SOURCE_DIR}
)

else(WIN32)
  file(GLOB _msquic_dll 
    ${msquic_release_SOURCE_DIR}/bin/libmsquic.so.*
  )
  # create a sym link
  execute_process(
    # for build
    COMMAND ln -sf ${_msquic_dll} ${CMAKE_BINARY_DIR}/libmsquic.so # for linking
    COMMAND ln -sf ${_msquic_dll} ${CMAKE_BINARY_DIR}/libmsquic.so.2 # for loading at runtime
    # for run
    COMMAND mkdir -p ${CMAKE_SOURCE_DIR}/target/debug/deps
    COMMAND cp ${_msquic_dll} ${CMAKE_SOURCE_DIR}/target/debug/libmsquic.so.2
    COMMAND cp ${_msquic_dll} ${CMAKE_SOURCE_DIR}/target/debug/deps/libmsquic.so.2
  )
  # copy files to output dir
  # file(COPY ${_msquic_dll} DESTINATION ${CMAKE_SOURCE_DIR}/target/debug)
endif(WIN32)

# downloading the msquic rust manual bindings.

# message(STATUS "download msquic_bind")
# file(DOWNLOAD
#   https://raw.githubusercontent.com/microsoft/msquic/v2.3.5/src/lib.rs
#   ${CMAKE_CURRENT_SOURCE_DIR}/crates/libs/c2/src/lib.rs
#   TLS_VERIFY ON
# )

if(WIN32)
# generate cert on windows:
find_program(pwsh_exe
  NAMES pwsh.exe
  REQUIRED
)

# get the test certificate
execute_process(COMMAND pwsh.exe -Command "Get-ChildItem Cert:\\CurrentUser\\My | Where-Object -Property FriendlyName -EQ -Value MsQuic-Test | Select-Object -ExpandProperty Thumbprint -First 1"
  OUTPUT_VARIABLE TEST_CERT_HASH
)

if(TEST_CERT_HASH STREQUAL "")
  message(Status "Generating TEST_CERT_HASH")
  execute_process(
    COMMAND pwsh.exe -Command "New-SelfSignedCertificate -DnsName $env:computername,localhost -FriendlyName MsQuic-Test -KeyUsageProperty Sign -KeyUsage DigitalSignature -CertStoreLocation cert:\\CurrentUser\\My -HashAlgorithm SHA256 -Provider \"Microsoft Software Key Storage Provider\" -KeyExportPolicy Exportable"
    COMMAND_ERROR_IS_FATAL ANY
  )
  execute_process(COMMAND pwsh.exe -Command "Get-ChildItem Cert:\\CurrentUser\\My | Where-Object -Property FriendlyName -EQ -Value MsQuic-Test | Select-Object -ExpandProperty Thumbprint -First 1"
    OUTPUT_VARIABLE TEST_CERT_HASH
    COMMAND_ERROR_IS_FATAL ANY
  )
endif()
  
message(STATUS "Using TEST_CERT_HASH ${TEST_CERT_HASH}")
endif(WIN32)