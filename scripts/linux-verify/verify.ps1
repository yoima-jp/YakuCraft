[Console]::InputEncoding = [System.Text.UTF8Encoding]::new()
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()
$OutputEncoding = [System.Text.UTF8Encoding]::new()

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "../..")).Path
$imageName = "yakucraft-linux-verify:local"
$dockerfile = Join-Path $PSScriptRoot "Dockerfile"
$buildContext = $PSScriptRoot
$nodeModulesVolume = "yakucraft-linux-verify-node-modules"
$cargoTargetVolume = "yakucraft-linux-verify-cargo-target"
$verificationPassed = $false

docker build --file $dockerfile --tag $imageName $buildContext
if ($LASTEXITCODE -ne 0) { throw "Docker image build failed with exit code $LASTEXITCODE." }

try {
    docker run --rm --init --shm-size=1g `
        --mount "type=bind,source=$repoRoot,target=/workspace" `
        --mount "type=volume,source=$nodeModulesVolume,target=/workspace/node_modules,volume-nocopy" `
        --mount "type=volume,source=$cargoTargetVolume,target=/opt/yakucraft-cargo-target,volume-nocopy" `
        --workdir /workspace `
        $imageName
    if ($LASTEXITCODE -ne 0) { throw "Ubuntu Linux verification failed with exit code $LASTEXITCODE." }
    $verificationPassed = $true
}
finally {
    docker image rm $imageName | Out-Null
    if ($verificationPassed) {
        docker volume rm $nodeModulesVolume $cargoTargetVolume | Out-Null
    }
}
