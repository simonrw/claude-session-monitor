.PHONY: help
help:
	commands: ios, macos, macos-gui


.PHONY: ios
ios: apps/ios/CsmIOS.xcodeproj
	bash ./apps/mac/build-xcframework.sh
	cd apps/ios && xcodebuild build

.PHONY: macos
macos: apps/mac/CsmCore.xcodeproj
	bash ./apps/mac/build-xcframework.sh
	bash ./apps/mac/build-app.sh

# Package the cross-platform egui GUI as a macOS .app via cargo-bundle
# (installed through mise). Output lands under the cargo target dir at
# release/bundle/osx/Claude Session Monitor.app - cargo-bundle prints the
# exact path when it finishes.
.PHONY: macos-gui
macos-gui:
	cd crates/gui && cargo bundle --release

apps/ios/CsmIOS.xcodeproj: apps/ios/project.yml
	cd apps/ios && xcodegen generate

apps/mac/CsmCore.xcodeproj: apps/mac/project.yml
	cd apps/mac && xcodegen generate

.PHONY: install-reporter
install-reporter:
	cargo install --path crates/reporter --locked

.PHONY: install-watcher
install-watcher:
	cargo install --path crates/watcher --locked
