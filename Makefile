.PHONY: help
help:
	commands: ios, macos


.PHONY: ios
ios: apps/ios/CsmIOS.xcodeproj
	bash ./apps/mac/build-xcframework.sh
	cd apps/ios && xcodebuild build

.PHONY: macos
macos: apps/mac/CsmCore.xcodeproj
	bash ./apps/mac/build-xcframework.sh
	bash ./apps/mac/build-app.sh

apps/ios/CsmIOS.xcodeproj: apps/ios/project.yml
	cd apps/ios && xcodegen generate

apps/mac/Csmmac.xcodeproj: apps/mac/project.yml
	cd apps/mac && xcodegen generate

.PHONY: install-reporter
install-reporter:
	cargo install --path crates/reporter --locked
